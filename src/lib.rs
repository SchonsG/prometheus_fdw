mod init;

use pgrx::warning;
use pgrx::{pg_sys, prelude::*, JsonB};
use reqwest::{self, Client};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::env;
use supabase_wrappers::prelude::*;
use tokio::runtime::Runtime;
pgrx::pg_module_magic!();
use pgrx::pg_sys::panic::ErrorReport;
use std::time::Duration;
use urlencoding::encode;

// convert response body text to rows
fn resp_to_rows(obj: &str, resp: &JsonValue, quals: &[Qual]) -> Vec<Row> {
    let mut result = Vec::new();

    match obj {
        "metrics" => {
            if let Some(result_array) = resp["data"]["result"].as_array() {
                for result_obj in result_array {
                    let metric_name_filter = quals
                        .iter()
                        .find(|qual| qual.field == "metric_name" && qual.operator == "=");
                    if let Some(metric_name) = metric_name_filter
                        .map(|qual| PrometheusFdw::value_to_promql_string(&qual.value))
                    {
                        let metric_labels = result_obj["metric"].clone();
                        if let Some(values_array) = result_obj["values"].as_array() {
                            for value_pair in values_array {
                                if let (Some(time_str), Some(value_str)) =
                                    (value_pair[0].as_i64(), value_pair[1].as_str())
                                {
                                    if let (metric_time, Ok(metric_value)) =
                                        (time_str, value_str.parse::<f64>())
                                    {
                                        let mut row = Row::new();
                                        row.push(
                                            "metric_name",
                                            Some(Cell::String(metric_name.clone())),
                                        );
                                        row.push(
                                            "metric_labels",
                                            Some(Cell::Json(JsonB(metric_labels.clone()))),
                                        );
                                        row.push("metric_time", Some(Cell::I64(metric_time)));
                                        row.push("metric_value", Some(Cell::F64(metric_value)));
                                        result.push(row);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {
            warning!("unsupported object: {}", obj);
        }
    }

    result
}

#[wrappers_fdw(
    version = "0.2.0",
    author = "Jay Kothari",
    website = "https://tembo.io",
    error_type = "PrometheusFdwError"
)]

pub(crate) struct PrometheusFdw {
    rt: Runtime,
    base_url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    bearer_token: Option<String>,
    client: Option<Client>,
    scan_result: Option<Vec<Row>>,
    tgt_cols: Vec<Column>,
}

enum PrometheusFdwError {}

impl From<PrometheusFdwError> for ErrorReport {
    fn from(_value: PrometheusFdwError) -> Self {
        ErrorReport::new(PgSqlErrorCode::ERRCODE_FDW_ERROR, "", "")
    }
}

type PrometheusFdwResult<T> = Result<T, PrometheusFdwError>;

impl PrometheusFdw {
    fn value_to_promql_string(value: &supabase_wrappers::interface::Value) -> String {
        match value {
            supabase_wrappers::interface::Value::Cell(cell) => match cell {
                supabase_wrappers::interface::Cell::String(s) => s.clone(),
                supabase_wrappers::interface::Cell::I32(i) => i.to_string(),
                _ => {
                    println!("Unsupported cell: {:?}", cell);
                    String::new()
                }
            },
            _ => {
                println!("Unsupported value: {:?}", value);
                String::new()
            }
        }
    }

    fn build_url(&self, obj: &str, options: &HashMap<String, String>, quals: &[Qual]) -> String {
        let step = if let Some(step_value) = options.get("step") {
            step_value.to_owned()
        } else {
            warning!("Using default value of 10m for step");
            let step_value = "10m".to_string();
            step_value
        };
        match obj {
            "metrics" => {
                let metric_name_filter = quals
                    .iter()
                    .find(|qual| qual.field == "metric_name" && qual.operator == "=");

                let lower_timestamp = quals.iter().find(|qual| {
                    qual.field == "metric_time" && qual.operator == ">" || qual.operator == ">="
                });

                let upper_timestamp = quals.iter().find(|qual| {
                    qual.field == "metric_time" && qual.operator == "<" || qual.operator == "<="
                });

                if let (Some(metric_name), Some(lower_timestamp), Some(upper_timestamp)) =
                    (metric_name_filter, lower_timestamp, upper_timestamp)
                {
                    let metric_name = Self::value_to_promql_string(&metric_name.value);
                    let lower_timestamp = Self::value_to_promql_string(&lower_timestamp.value);
                    let upper_timestamp = Self::value_to_promql_string(&upper_timestamp.value);
                    let ret = format!(
                        "{}/api/v1/query_range?query={}&start={}&end={}&step={}",
                        self.base_url.as_ref().unwrap(),
                        encode(&metric_name),
                        lower_timestamp,
                        upper_timestamp,
                        step
                    );
                    ret
                } else {
                    println!("filters not found in quals");
                    "".to_string()
                }
            }
            _ => {
                println!("Unsupported object: {}", obj);
                "".to_string()
            }
        }
    }
}

impl ForeignDataWrapper<PrometheusFdwError> for PrometheusFdw {
    fn new(server: ForeignServer) -> PrometheusFdwResult<Self> {
        let mut ret = Self {
            rt: create_async_runtime().expect("failed to create async runtime"),
            base_url: None,
            username: None,
            password: None,
            bearer_token: None,
            client: None,
            tgt_cols: Vec::new(),
            scan_result: None,
        };

        let base_url = if let Some(prom_url) = server.options.get("base_url") {
            prom_url.to_owned()
        } else {
            warning!("Cannot find prometheus base url in options");
            let prom_url = env::var("PROMETHEUS_BASE_URL").unwrap();
            prom_url
        };

        ret.base_url = Some(base_url);
        ret.client = Some(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to build client"),
        );

        Ok(ret)
    }

    fn begin_scan(
        &mut self,
        quals: &[Qual],
        columns: &[Column],
        _sorts: &[Sort],
        _limit: &Option<Limit>,
        options: &HashMap<String, String>,
    ) -> PrometheusFdwResult<()> {
        let obj = require_option("object", options).expect("invalid option");

        self.scan_result = None;
        self.tgt_cols = columns.to_vec();

        if let Some(client) = &self.client {
            let mut result = Vec::new();

            if obj == "metrics" {
                let url = self.build_url(&obj, options, quals);
                let resp;

                if let Some(bearer_token) = &self.bearer_token {
                    // Create a RequestBuilder and set the bearer token
                    let request = client.get(&url).bearer_auth(bearer_token);
                    resp = self.rt.block_on(async { request.send().await }).ok();
                } else if let (Some(username), Some(password)) = (&self.username, &self.password) {
                    // Create a RequestBuilder with basic auth
                    let request = client.get(&url).basic_auth(username, Some(password));
                    resp = self.rt.block_on(async { request.send().await }).ok();
                } else {
                    // Send a request without authentication
                    resp = self
                        .rt
                        .block_on(async { client.get(&url).send().await })
                        .ok();
                }

                // Assuming resp is of type Result<Response, reqwest::Error>
                match resp {
                    Some(response) => {
                        let body_result = self.rt.block_on(async { response.text().await });
                        match body_result {
                            Ok(body) => {
                                // `body` is a String here
                                let json: JsonValue = serde_json::from_str(&body).unwrap();
                                result = resp_to_rows(&obj, &json, &quals);
                            }
                            Err(e) => {
                                warning!("failed to get body: {}", e);
                            }
                        }
                    }
                    None => {
                        // Handle the case when resp is None
                        warning!("No response received");
                    }
                }
            }

            self.scan_result = Some(result);
        }
        Ok(())
    }

    fn iter_scan(&mut self, row: &mut Row) -> PrometheusFdwResult<Option<()>> {
        if let Some(ref mut result) = self.scan_result {
            if !result.is_empty() {
                let scanned = result.drain(0..1).last().map(|src_row| {
                    row.replace_with(src_row);
                });
                return Ok(scanned);
            }
        }
        Ok(None)
    }

    fn end_scan(&mut self) -> PrometheusFdwResult<()> {
        self.scan_result.take();
        Ok(())
    }

    fn validator(
        options: Vec<Option<String>>,
        catalog: Option<pg_sys::Oid>,
    ) -> PrometheusFdwResult<()> {
        if let Some(oid) = catalog {
            if oid == FOREIGN_TABLE_RELATION_ID {
                let _ = check_options_contain(&options, "object");
            }
        }
        Ok(())
    }
}
