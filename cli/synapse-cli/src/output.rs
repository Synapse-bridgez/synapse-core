use serde::Serialize;
use std::fmt::Display;

pub fn format_output<T: Serialize + Display>(data: T, json: bool) {
    if json {
        match serde_json::to_string(&data) {
            Ok(output) => println!("{}", output),
            Err(e) => eprintln!("Failed to serialize as JSON: {}", e),
        }
    } else {
        println!("{}", data);
    }
}
