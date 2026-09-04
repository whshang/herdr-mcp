use serde_json::{Map, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const SCHEMA_TTL: Duration = Duration::from_secs(60);
const SCHEMA_LOAD_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_SCHEMA_BYTES: usize = 8 * 1024 * 1024;

static CACHE: OnceLock<Mutex<Option<CachedSchema>>> = OnceLock::new();
static REFRESHING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
struct CachedSchema {
    loaded_at: Instant,
    registry: Arc<SchemaRegistry>,
}

#[derive(Debug, Clone)]
pub struct SchemaRegistry {
    methods: Vec<MethodSchema>,
    defs: Map<String, Value>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MethodSchema {
    pub method: String,
    pub properties: Map<String, Value>,
    pub required: Vec<String>,
    pub empty: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ValidationIssue {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ValidationResult {
    pub ok: bool,
    pub errors: Vec<ValidationIssue>,
    pub warnings: Vec<ValidationIssue>,
}

pub fn list_methods(query: &str) -> Result<Vec<MethodSchema>, String> {
    let registry = load_registry(false)?;
    let query = query.to_ascii_lowercase();
    Ok(registry
        .methods
        .iter()
        .filter(|method| query.is_empty() || method.method.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect())
}

pub fn validate_method_params(method: &str, params: &Value) -> Result<ValidationResult, String> {
    let registry = load_registry(false)?;
    Ok(validate_with_registry(&registry, method, params))
}

/// Warm the live Herdr schema without delaying server startup.
pub fn prewarm_async() {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if cache
        .lock()
        .ok()
        .is_some_and(|guard| guard.as_ref().is_some())
    {
        return;
    }
    start_background_refresh();
}

fn validate_with_registry(
    registry: &SchemaRegistry,
    method: &str,
    params: &Value,
) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let Some(schema) = registry
        .methods
        .iter()
        .find(|candidate| candidate.method == method)
    else {
        warnings.push(ValidationIssue {
            name: "method".to_owned(),
            message: format!(
                "method \"{method}\" not found in herdr schema - passed through unvalidated"
            ),
        });
        return ValidationResult {
            ok: true,
            errors,
            warnings,
        };
    };

    let Some(given) = params.as_object() else {
        errors.push(ValidationIssue {
            name: "params".to_owned(),
            message: "params must be a JSON object".to_owned(),
        });
        return ValidationResult {
            ok: false,
            errors,
            warnings,
        };
    };

    if schema.empty {
        if !given.is_empty() {
            warnings.push(ValidationIssue {
                name: "params".to_owned(),
                message: format!(
                    "method takes no params; got: {}",
                    given.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        return ValidationResult {
            ok: true,
            errors,
            warnings,
        };
    }

    for name in &schema.required {
        if !given.contains_key(name) {
            errors.push(ValidationIssue {
                name: name.clone(),
                message: format!("missing required param \"{name}\""),
            });
        }
    }

    for (name, value) in given {
        let Some(property) = schema.properties.get(name) else {
            errors.push(ValidationIssue {
                name: name.clone(),
                message: format!("unknown param \"{name}\" (not in schema)"),
            });
            continue;
        };
        let Some(primitive) = primitive_type(property, &registry.defs) else {
            continue;
        };
        if !value.is_null() && !matches_type(value, &primitive.types) {
            errors.push(ValidationIssue {
                name: name.clone(),
                message: format!(
                    "param \"{name}\" should be {}, got {}",
                    primitive.types.join("|"),
                    value_type(value)
                ),
            });
            continue;
        }
        if let Some(allowed) = primitive.allowed
            && value.is_string()
            && !allowed.iter().any(|candidate| candidate == value)
        {
            errors.push(ValidationIssue {
                name: name.clone(),
                message: format!(
                    "param \"{name}\" must be one of [{}], got {}",
                    allowed
                        .iter()
                        .map(Value::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    value
                ),
            });
        }
    }

    ValidationResult {
        ok: errors.is_empty(),
        errors,
        warnings,
    }
}

#[derive(Debug)]
struct PrimitiveType {
    types: Vec<String>,
    allowed: Option<Vec<Value>>,
}

fn primitive_type(property: &Value, defs: &Map<String, Value>) -> Option<PrimitiveType> {
    let object = property.as_object()?;
    if object.get("anyOf").is_some_and(Value::is_array) {
        return None;
    }

    let resolved = object
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| resolve_ref(reference, defs))
        .and_then(Value::as_object);
    let effective = resolved.unwrap_or(object);

    let allowed = effective.get("enum").and_then(Value::as_array).cloned();
    let types = match effective.get("type") {
        Some(Value::String(value)) => vec![value.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ if allowed.is_some() => vec!["string".to_owned()],
        _ => return None,
    };

    Some(PrimitiveType { types, allowed })
}

fn matches_type(value: &Value, types: &[String]) -> bool {
    types.iter().any(|kind| match kind.as_str() {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value
            .as_i64()
            .map(|_| true)
            .or_else(|| value.as_u64().map(|_| true))
            .unwrap_or(false),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    })
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn load_registry(force: bool) -> Result<Arc<SchemaRegistry>, String> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache
            .lock()
            .map_err(|_| "schema cache lock poisoned".to_owned())?;
        if !force && let Some(cached) = guard.as_ref() {
            let registry = Arc::clone(&cached.registry);
            if cached.loaded_at.elapsed() < SCHEMA_TTL {
                return Ok(registry);
            }
            drop(guard);
            start_background_refresh();
            return Ok(registry);
        }
    }

    match load_live_registry() {
        Ok(registry) => {
            let registry = Arc::new(registry);
            let mut guard = cache
                .lock()
                .map_err(|_| "schema cache lock poisoned".to_owned())?;
            *guard = Some(CachedSchema {
                loaded_at: Instant::now(),
                registry: Arc::clone(&registry),
            });
            Ok(registry)
        }
        Err(error) => {
            let guard = cache
                .lock()
                .map_err(|_| "schema cache lock poisoned".to_owned())?;
            if let Some(cached) = guard.as_ref() {
                Ok(Arc::clone(&cached.registry))
            } else {
                Err(error)
            }
        }
    }
}

fn start_background_refresh() {
    if REFRESHING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    thread::spawn(|| {
        if let Ok(registry) = load_live_registry()
            && let Ok(mut guard) = CACHE.get_or_init(|| Mutex::new(None)).lock()
        {
            *guard = Some(CachedSchema {
                loaded_at: Instant::now(),
                registry: Arc::new(registry),
            });
        }
        REFRESHING.store(false, Ordering::Release);
    });
}

fn load_live_registry() -> Result<SchemaRegistry, String> {
    let raw = run_schema_command()?;
    let document: Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("cannot parse `herdr api schema --json`: {error}"))?;
    registry_from_document(&document)
}

fn run_schema_command() -> Result<Vec<u8>, String> {
    let mut child = Command::new("herdr")
        .args(["api", "schema", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start `herdr api schema --json`: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cannot capture herdr schema stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "cannot capture herdr schema stderr".to_owned())?;
    let stdout_reader = thread::spawn(move || read_stream(stdout));
    let stderr_reader = thread::spawn(move || read_stream(stderr));
    let started = Instant::now();

    let status = loop {
        match child
            .try_wait()
            .map_err(|error| format!("cannot wait for herdr schema command: {error}"))?
        {
            Some(status) => break status,
            None if started.elapsed() >= SCHEMA_LOAD_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!(
                    "`herdr api schema --json` exceeded {}ms",
                    SCHEMA_LOAD_TIMEOUT.as_millis()
                ));
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| "herdr schema stdout reader panicked".to_owned())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "herdr schema stderr reader panicked".to_owned())??;
    if !status.success() {
        return Err(format!(
            "`herdr api schema --json` failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    if stdout.len() > MAX_SCHEMA_BYTES {
        return Err(format!("herdr schema exceeded {MAX_SCHEMA_BYTES} bytes"));
    }
    Ok(stdout)
}

fn read_stream(mut stream: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    stream
        .read_to_end(&mut output)
        .map_err(|error| format!("cannot read herdr schema process output: {error}"))?;
    Ok(output)
}

fn registry_from_document(document: &Value) -> Result<SchemaRegistry, String> {
    let request = document
        .pointer("/schemas/request")
        .and_then(Value::as_object)
        .ok_or_else(|| "herdr schema missing schemas.request".to_owned())?;
    let one_of = request
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "herdr schema missing schemas.request.oneOf".to_owned())?;
    let defs = request
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut methods = Vec::new();

    for item in one_of {
        let Some(properties) = item.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let Some(method) = properties
            .get("method")
            .and_then(|value| value.get("const"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let params_shape = properties
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let resolved = params_shape
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| resolve_ref(reference, &defs))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or(params_shape);
        let param_properties = resolved
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let required = resolved
            .get("required")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let empty = param_properties.is_empty()
            && matches!(
                resolved.get("type").and_then(Value::as_str),
                None | Some("object")
            );

        methods.push(MethodSchema {
            method: method.to_owned(),
            properties: param_properties,
            required,
            empty,
        });
    }

    if methods.is_empty() {
        return Err("herdr schema contains no request methods".to_owned());
    }
    Ok(SchemaRegistry { methods, defs })
}

fn resolve_ref<'a>(reference: &str, defs: &'a Map<String, Value>) -> Option<&'a Value> {
    let name = reference.rsplit('/').next().unwrap_or(reference);
    defs.get(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registry() -> SchemaRegistry {
        let document = json!({
            "schemas": {
                "request": {
                    "oneOf": [
                        {
                            "properties": {
                                "method": {"const": "ping"},
                                "params": {"$ref": "#/schemas/request/$defs/PingParams"}
                            }
                        },
                        {
                            "properties": {
                                "method": {"const": "agent.start"},
                                "params": {"$ref": "#/schemas/request/$defs/AgentStartParams"}
                            }
                        }
                    ],
                    "$defs": {
                        "PingParams": {"type": "object", "properties": {}},
                        "AgentStartParams": {
                            "type": "object",
                            "properties": {
                                "workspace_id": {"type": "string"},
                                "count": {"type": "integer"},
                                "kind": {"enum": ["pi", "grok"]}
                            },
                            "required": ["workspace_id"]
                        }
                    }
                }
            }
        });
        registry_from_document(&document).unwrap()
    }

    #[test]
    fn parses_method_param_schema() {
        let registry = registry();
        assert_eq!(registry.methods.len(), 2);
        assert!(registry.methods[0].empty);
        assert_eq!(registry.methods[1].required, vec!["workspace_id"]);
        assert!(registry.methods[1].properties.contains_key("kind"));
    }

    #[test]
    fn validates_required_type_enum_and_unknown_params() {
        let registry = registry();
        let valid = validate_with_registry(
            &registry,
            "agent.start",
            &json!({"workspace_id": "w1", "count": 2, "kind": "pi", "future": true}),
        );
        assert!(!valid.ok);
        assert_eq!(valid.errors.len(), 1);
        assert!(valid.warnings.is_empty());

        let invalid = validate_with_registry(
            &registry,
            "agent.start",
            &json!({"count": "two", "kind": "other"}),
        );
        assert!(!invalid.ok);
        assert_eq!(invalid.errors.len(), 3);
    }

    #[test]
    fn unknown_method_passes_with_warning() {
        let result = validate_with_registry(&registry(), "future.method", &json!({}));
        assert!(result.ok);
        assert_eq!(result.warnings.len(), 1);
    }
}
