#![cfg(not(target_arch = "wasm32"))]

//! Integration tests for discovery and the serialized application contract.

#[allow(
    dead_code,
    reason = "without the python feature, only the export registry reaches the fixture items"
)]
mod fixtures;

use std::collections::BTreeSet;
use std::ffi::CStr;

use bimetal::ir::{BufferElement, FieldDef, Manifest, TypeRef, TypeShape, VariantPayload};
use bimetal::runtime::ContractError;
use serde_json::json;

use crate::fixtures::lookup;

fn discovered_manifest() -> Manifest {
    let result = fixtures::bimetal_contract();
    assert_eq!(result.status, bimetal::ir::discovery::SUCCESS);
    assert!(!result.payload.is_null());

    // SAFETY: discovery returns a live NUL-terminated allocation. This test
    // releases it exactly once with the paired free function.
    let payload = unsafe { CStr::from_ptr(result.payload) }
        .to_str()
        .expect("the discovery payload must be UTF-8")
        .to_owned();
    // SAFETY: `result.payload` came from `bimetal_contract` above.
    unsafe { fixtures::bimetal_contract_free(result.payload) };

    serde_json::from_str(&payload).expect("the discovery payload must be a manifest")
}

fn field_named<'a>(shape: &'a TypeShape, name: &str) -> &'a FieldDef {
    let TypeShape::Struct { fields } = shape else {
        panic!("expected a struct shape");
    };
    fields
        .iter()
        .find(|field| field.wire_name == name)
        .unwrap_or_else(|| panic!("missing field {name}"))
}

#[test]
fn discovery_returns_the_complete_application_contract() {
    let mut manifest = discovered_manifest();
    // The Arrow fixtures link only with the `arrow` feature, and
    // `arrow_boundaries_record_the_arrow_value` checks them; the extra
    // exports belong to the Python suite.
    let optional_modules = [
        "contract::fixtures::chain",
        "contract::fixtures::columns",
        "contract::fixtures::defaults",
        "contract::fixtures::extra",
        "contract::fixtures::gates",
        "contract::fixtures::streams",
    ];
    manifest
        .types
        .retain(|item| !optional_modules.contains(&item.rust_module.as_str()));
    manifest
        .functions
        .retain(|item| !optional_modules.contains(&item.rust_module.as_str()));
    manifest
        .errors
        .retain(|item| !optional_modules.contains(&item.rust_module.as_str()));
    manifest
        .resources
        .retain(|item| !optional_modules.contains(&item.rust_module.as_str()));

    assert_eq!(manifest.package_name, env!("CARGO_PKG_NAME"));
    assert_eq!(manifest.package_version, env!("CARGO_PKG_VERSION"));
    let native_names: BTreeSet<&str> = manifest
        .functions
        .iter()
        .map(|item| item.native_name.as_str())
        .chain(
            manifest
                .resources
                .iter()
                .map(|item| item.native_name.as_str()),
        )
        .collect();
    assert_eq!(
        native_names.len(),
        manifest.functions.len() + manifest.resources.len()
    );
    assert!(
        manifest
            .types
            .iter()
            .all(|item| item.owner.0 == env!("CARGO_PKG_NAME")
                && item.rust_module.starts_with("contract::fixtures"))
    );
    assert!(manifest.functions.iter().all(|item| {
        item.owner.0 == env!("CARGO_PKG_NAME") && item.rust_module == "contract::fixtures"
    }));
    assert_eq!(
        manifest
            .types
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        [
            "JobContext",
            "JobEvent",
            "JobId",
            "JobRequest",
            "JobResult",
            "JobStep",
            "JobWindow",
            "RunMode",
            "WideNumbers",
            "InlineValue",
        ]
    );
    assert_eq!(
        manifest
            .functions
            .iter()
            .map(|item| item.callable.host_name.as_str())
            .collect::<Vec<_>>(),
        [
            "checkCountLater",
            "doubleLater",
            "echoStep",
            "echoU64",
            "echoWideNumbers",
            "echoWindow",
            "executeJob",
            "fileName",
            "holdsInterpreterLock",
            "label",
            "renameJob",
            "reverseBytes",
            "scaleSamples",
        ]
    );
    assert_eq!(
        manifest
            .resources
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["Counter", "Ledger", "LockProbe"]
    );
    assert_eq!(
        manifest
            .constants
            .iter()
            .map(|item| item.host_name.as_str())
            .collect::<Vec<_>>(),
        ["CONTRACT_REVISION", "DEFAULT_LABEL"]
    );
    assert_eq!(
        manifest
            .errors
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["JobError"]
    );

    let encoded = serde_json::to_string(&manifest).expect("manifest must serialize");
    let decoded: Manifest = serde_json::from_str(&encoded).expect("manifest must deserialize");
    assert_eq!(decoded, manifest);

    let inline = lookup::model(&manifest, "InlineValue");
    assert_eq!(inline.rust_module, "contract::fixtures::inline");
    assert_eq!(
        manifest
            .namespace_rule()
            .namespace(&inline.owner, &inline.rust_module)
            .python_segments(),
        ["fixtures", "inline"]
    );
}

#[test]
fn a_field_default_is_the_rust_default_of_its_type() {
    let manifest = discovered_manifest();
    let policy = lookup::model(&manifest, "RetryPolicy");
    let backoff = json!({"cap": 60, "factor": 2.0});

    let defaults = [
        ("delays", json!([])),
        ("backoff", backoff.clone()),
        ("share", json!(0.0)),
        ("then", json!("give_up")),
        ("codes", json!({"busy": 75, "killed": 137, "timeout": 124})),
        ("limit", json!({"nanos": 0, "secs": 0})),
    ];
    for (name, default) in defaults {
        let field = field_named(&policy.shape, name);
        assert!(!field.required, "`{name}` may be absent");
        assert_eq!(field.default, Some(default), "the default of `{name}`");
    }

    let TypeShape::TaggedEnum { variants, .. } = &lookup::model(&manifest, "Step").shape else {
        panic!("expected a tagged enum");
    };
    assert_eq!(variants[0].fields[0].default, Some(backoff.clone()));
    // The derive reads the attribute where a macro forwards it.
    let declared = lookup::model(&manifest, "Declared");
    assert_eq!(
        field_named(&declared.shape, "backoff").default,
        Some(backoff)
    );
}

#[test]
fn a_feature_gate_decides_whether_a_member_is_in_the_contract() {
    let manifest = discovered_manifest();
    let till = lookup::resource(&manifest, "Till");
    let methods: Vec<&str> = till
        .methods
        .iter()
        .map(|method| method.callable.rust_name.as_str())
        .collect();
    let factories: Vec<&str> = till
        .factories
        .iter()
        .map(|factory| factory.rust_name.as_str())
        .collect();
    let receipt = manifest.types.iter().find(|item| item.name == "Receipt");

    if cfg!(feature = "alloy") {
        assert_eq!(methods, ["take", "receipt", "settle"]);
        assert_eq!(factories, ["holding"]);
        assert!(
            receipt.is_some(),
            "the gated method's type is in the contract"
        );
    } else {
        assert_eq!(methods, ["take", "plain_total"]);
        assert!(factories.is_empty());
        assert!(receipt.is_none());
    }
    // A gated constructor never becomes the class constructor.
    let constructor = till.constructor.as_ref().expect("an ungated constructor");
    assert_eq!(constructor.rust_name, "new");
}

#[test]
fn request_fields_preserve_names_constraints_defaults_and_boundaries() {
    let manifest = discovered_manifest();
    let request = lookup::model(&manifest, "JobRequest");

    let display_name = field_named(&request.shape, "displayName");
    assert_eq!(display_name.rust_name, "display_name");
    assert_eq!(display_name.constraints.min_length, Some(2));
    assert_eq!(display_name.constraints.max_length, Some(64));

    let count = field_named(&request.shape, "count");
    assert_eq!(count.constraints.ge, Some(1));
    assert_eq!(count.constraints.le, Some(100));
    let dry_run = field_named(&request.shape, "dryRun");
    assert!(!dry_run.required);
    assert_eq!(dry_run.default, Some(json!(false)));

    let context = field_named(&request.shape, "context");
    assert!(!context.required);
    assert_eq!(
        context.ty,
        TypeRef::Option {
            item: Box::new(TypeRef::Named {
                identity: lookup::model(&manifest, "JobContext").identity(),
            }),
        }
    );
}

#[test]
fn context_fields_cover_supported_composite_types() {
    let manifest = discovered_manifest();
    let context = lookup::model(&manifest, "JobContext");

    assert_eq!(
        field_named(&context.shape, "createdAt").ty,
        TypeRef::DateTime
    );
    assert_eq!(field_named(&context.shape, "metadata").ty, TypeRef::Json);
    assert_eq!(
        field_named(&context.shape, "labels").ty,
        TypeRef::Map {
            value: Box::new(TypeRef::String),
        }
    );
    assert_eq!(
        field_named(&context.shape, "range").ty,
        TypeRef::Tuple {
            items: vec![
                TypeRef::Int {
                    signed: true,
                    bits: 32,
                },
                TypeRef::Int {
                    signed: true,
                    bits: 32,
                },
            ],
        }
    );
    assert_eq!(
        field_named(&context.shape, "note").ty,
        TypeRef::Option {
            item: Box::new(TypeRef::String),
        }
    );
    assert_eq!(
        field_named(&context.shape, "fingerprint").ty,
        TypeRef::List {
            item: Box::new(TypeRef::Int {
                signed: false,
                bits: 8,
            }),
        }
    );
}

#[test]
fn enums_and_aliases_preserve_their_wire_shapes() {
    let manifest = discovered_manifest();
    let run_mode = lookup::model(&manifest, "RunMode");
    let TypeShape::StringEnum { variants } = &run_mode.shape else {
        panic!("RunMode must be a string enum");
    };
    assert_eq!(
        variants
            .iter()
            .map(|variant| variant.wire_name.as_str())
            .collect::<Vec<_>>(),
        ["fast", "deterministic"]
    );

    let event = lookup::model(&manifest, "JobEvent");
    let TypeShape::TaggedEnum { tag, variants } = &event.shape else {
        panic!("JobEvent must be a tagged enum");
    };
    assert_eq!(tag, "kind");
    assert_eq!(variants[0].wire_name, "completed");
    assert_eq!(variants[0].fields[0].wire_name, "acceptedCount");

    assert_eq!(
        lookup::model(&manifest, "JobId").shape,
        TypeShape::Alias {
            target: TypeRef::String,
        }
    );

    let step = lookup::model(&manifest, "JobStep");
    let TypeShape::ExternalEnum { variants } = &step.shape else {
        panic!("JobStep must be an externally tagged enum");
    };
    assert_eq!(
        variants
            .iter()
            .map(|variant| variant.wire_name.as_str())
            .collect::<Vec<_>>(),
        ["queued", "running", "stopped"]
    );
    assert_eq!(variants[0].payload, VariantPayload::Unit);
    // A box crosses as the value inside it.
    assert_eq!(
        variants[1].payload,
        VariantPayload::Value {
            ty: TypeRef::Named {
                identity: lookup::model(&manifest, "JobRequest").identity(),
            },
        }
    );
    let VariantPayload::Fields { fields } = &variants[2].payload else {
        panic!("JobStep::Stopped must carry fields");
    };
    assert_eq!(fields[0].wire_name, "acceptedCount");
    assert_eq!(fields[0].constraints.ge, Some(1));
}

#[test]
fn durations_arrays_and_paths_cross_as_serde_writes_them() {
    let manifest = discovered_manifest();
    let window = &lookup::model(&manifest, "JobWindow").shape;
    assert_eq!(field_named(window, "timeout").ty, TypeRef::Duration);
    let byte = TypeRef::Int {
        signed: false,
        bits: 8,
    };
    assert_eq!(
        field_named(window, "hours").ty,
        TypeRef::Tuple {
            items: vec![byte.clone(), byte],
        }
    );
    assert_eq!(field_named(window, "starts").ty, TypeRef::Date);
    let file_name = lookup::function(&manifest, "fileName");
    assert_eq!(file_name.callable.params[0].ty, TypeRef::String);
}

#[cfg(feature = "alloy")]
#[test]
fn alloy_types_cross_as_hex_strings() {
    let manifest = discovered_manifest();
    let transfer = &lookup::model(&manifest, "Transfer").shape;
    for name in ["from", "amount", "transaction", "nonce", "change"] {
        assert_eq!(field_named(transfer, name).ty, TypeRef::String, "{name}");
    }
}

#[test]
fn functions_preserve_names_boundaries_and_typed_errors() {
    let manifest = discovered_manifest();
    let execute = lookup::function(&manifest, "executeJob");
    assert_eq!(execute.callable.rust_name, "execute_job");
    assert!(matches!(execute.returns, TypeRef::Named { .. }));
    assert_eq!(execute.callable.error, Some(manifest.errors[0].identity()));
    assert_eq!(execute.callable.params[1].ty, TypeRef::Bytes);
    assert_eq!(
        execute.callable.params[2].ty,
        TypeRef::Buffer {
            element: BufferElement::F64,
        }
    );

    let reverse = lookup::function(&manifest, "reverseBytes");
    assert_eq!(reverse.callable.params[0].ty, TypeRef::Bytes);
    assert_eq!(reverse.returns, TypeRef::Bytes);

    let scale = lookup::function(&manifest, "scaleSamples");
    assert_eq!(
        scale.callable.params[0].ty,
        TypeRef::Buffer {
            element: BufferElement::F64,
        }
    );
    assert_eq!(scale.callable.params[1].host_name, "factor");
    assert_eq!(
        scale.returns,
        TypeRef::Buffer {
            element: BufferElement::F64,
        }
    );
}

#[test]
fn resources_constants_and_error_codes_are_exact() {
    let manifest = discovered_manifest();
    let counter = &manifest.resources[0];

    assert_eq!(
        counter
            .constructors()
            .map(|item| item.host_name.as_str())
            .collect::<Vec<_>>(),
        ["new", "fromZero"]
    );
    assert_eq!(
        counter
            .methods
            .iter()
            .map(|item| item.callable.host_name.as_str())
            .collect::<Vec<_>>(),
        ["add", "addMany"]
    );
    let constructor = counter.constructor.as_ref().expect("class constructor");
    assert_eq!(constructor.error, Some(manifest.errors[0].identity()));
    assert!(
        counter
            .methods
            .iter()
            .all(|method| method.callable.host_name != "reset")
    );

    assert_eq!(manifest.constants[0].value, serde_json::json!(7));
    assert_eq!(
        manifest.constants[1].value,
        serde_json::json!("integration")
    );
    assert_eq!(fixtures::JobError::NegativeStart.code(), "negative_start");
}

#[test]
fn async_exports_are_marked_and_async_constructors_become_factories() {
    let manifest = discovered_manifest();
    assert!(lookup::function(&manifest, "doubleLater").callable.is_async);
    assert!(!lookup::function(&manifest, "echoU64").callable.is_async);

    let ledger = lookup::resource(&manifest, "Ledger");
    assert!(ledger.constructor.is_none());
    assert_eq!(
        ledger
            .factories
            .iter()
            .map(|factory| (factory.host_name.as_str(), factory.is_async))
            .collect::<Vec<_>>(),
        [("open", true)]
    );
    assert_eq!(
        ledger
            .methods
            .iter()
            .map(|method| (method.callable.host_name.as_str(), method.callable.is_async))
            .collect::<Vec<_>>(),
        [
            ("record", true),
            ("meet", true),
            ("hold", true),
            ("count", false)
        ]
    );
}

#[cfg(feature = "arrow-api")]
#[test]
fn arrow_boundaries_record_the_arrow_value() {
    use bimetal::ir::ArrowKind;

    let manifest = discovered_manifest();
    let arrow = |value| TypeRef::Arrow { value };
    let first_batch = lookup::function(&manifest, "firstBatch");
    assert_eq!(first_batch.callable.params[0].ty, arrow(ArrowKind::Table));
    assert_eq!(first_batch.returns, arrow(ArrowKind::RecordBatch));
    let column_at = lookup::function(&manifest, "columnAt");
    assert_eq!(
        column_at.callable.params[0].ty,
        arrow(ArrowKind::RecordBatch)
    );
    assert_eq!(column_at.returns, arrow(ArrowKind::Array));
    assert_eq!(
        lookup::function(&manifest, "tagSchema").returns,
        arrow(ArrowKind::Schema)
    );
    assert!(matches!(
        lookup::function(&manifest, "summarizeTable").returns,
        TypeRef::Named { .. }
    ));
    #[cfg(feature = "python")]
    {
        assert_eq!(
            lookup::function(&manifest, "count").returns,
            arrow(ArrowKind::Stream)
        );
        assert_eq!(
            lookup::function(&manifest, "sumLater").callable.params[0].ty,
            arrow(ArrowKind::Stream)
        );
    }
}
