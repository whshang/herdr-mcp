use crate::herdr::HerdrClient;
use crate::inspect;
use crate::schema::{self, MethodSchema, ValidationIssue};
use serde_json::{Value, json};

const SCHEMA_SOURCE: &str = "herdr api schema --json (live, 60s cache)";

pub fn inspect(client: &HerdrClient) -> Value {
    inspect::inspect_core(client)
}

pub fn methods(query: &str) -> Value {
    match schema::list_methods(query) {
        Ok(methods) => json!({
            "ok": true,
            "count": methods.len(),
            "methods": methods.iter().map(method_json).collect::<Vec<_>>(),
            "source": SCHEMA_SOURCE,
        }),
        Err(error) => json!({
            "ok": false,
            "reason": "schema_unavailable",
            "message": error,
        }),
    }
}

pub fn call(client: &HerdrClient, method: &str, params: Value) -> Value {
    if !params.is_object() {
        return json!({
            "ok": false,
            "code": "invalid_params",
            "method": method,
            "errors": ["params must be a JSON object"],
        });
    }

    let validation = match schema::validate_method_params(method, &params) {
        Ok(validation) => validation,
        Err(error) => {
            return json!({
                "ok": false,
                "reason": "schema_unavailable",
                "method": method,
                "message": error,
            });
        }
    };

    if !validation.ok {
        return json!({
            "ok": false,
            "code": "invalid_params",
            "method": method,
            "errors": validation.errors.iter().map(issue_json).collect::<Vec<_>>(),
            "warnings": validation.warnings.iter().map(issue_json).collect::<Vec<_>>(),
        });
    }

    match client.call(method, params) {
        Ok(result) => {
            let warnings = validation
                .warnings
                .iter()
                .map(issue_json)
                .collect::<Vec<_>>();
            if warnings.is_empty() {
                json!({"ok": true, "result": result})
            } else {
                json!({"ok": true, "result": result, "warnings": warnings})
            }
        }
        Err(error) => json!({
            "ok": false,
            "code": error.code,
            "message": error.message,
            "method": method,
        }),
    }
}

fn method_json(method: &MethodSchema) -> Value {
    json!({
        "method": method.method,
        "params": {
            "properties": method.properties,
            "required": method.required,
            "empty": method.empty,
        },
    })
}

fn issue_json(issue: &ValidationIssue) -> Value {
    json!({
        "name": issue.name,
        "message": issue.message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_object_params_before_schema_or_socket() {
        let client = HerdrClient::new("/path/that/does/not/exist");
        let result = call(&client, "ping", json!([1, 2, 3]));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "invalid_params");
    }
}
