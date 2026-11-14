//! The contract: every declaration that one application exports.

use std::collections::BTreeSet;
use std::{fmt, iter, slice};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::NamespaceRule;

/// The complete application contract consumed by the package generators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// Cargo package that owns the application entry point.
    pub package_name: String,
    /// Version shared by the Cargo, Python, and npm packages.
    pub package_version: String,
    /// Linked model declarations.
    pub types: Vec<TypeDef>,
    /// Linked typed-error declarations.
    pub errors: Vec<ErrorDef>,
    /// Linked free functions.
    pub functions: Vec<FunctionDef>,
    /// Linked stateful resources.
    pub resources: Vec<ResourceDef>,
    /// Linked constants and statics.
    pub constants: Vec<ConstantDef>,
}

impl Manifest {
    /// Return the rule that derives each declaration's generated namespace
    /// from the name of its Cargo package.
    ///
    /// The rule depends on every declaring package, so derive it once and
    /// reuse it rather than recomputing it per declaration.
    #[must_use]
    pub fn namespace_rule(&self) -> NamespaceRule {
        let mut packages = BTreeSet::from([self.package_name.as_str()]);
        packages.extend(self.types.iter().map(|item| item.owner.0.as_str()));
        packages.extend(self.errors.iter().map(|item| item.owner.0.as_str()));
        packages.extend(self.functions.iter().map(|item| item.owner.0.as_str()));
        packages.extend(self.resources.iter().map(|item| item.owner.0.as_str()));
        packages.extend(self.constants.iter().map(|item| item.owner.0.as_str()));
        NamespaceRule::from_packages(packages)
    }

    /// Return every type reference that a declaration uses directly: the
    /// fields of the models, the parameters and returns of the callables, and
    /// the types of the constants. [`TypeRef::children`] reaches the
    /// references inside each one.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        self.types
            .iter()
            .flat_map(TypeDef::type_refs)
            .chain(self.functions.iter().flat_map(FunctionDef::type_refs))
            .chain(self.resources.iter().flat_map(ResourceDef::type_refs))
            .chain(self.constants.iter().map(|constant| &constant.ty))
    }

    /// Return every callable: each function's, then each resource's
    /// constructors and methods.
    pub fn callables(&self) -> impl Iterator<Item = &CallableDef> {
        self.functions
            .iter()
            .map(|function| &function.callable)
            .chain(self.resources.iter().flat_map(ResourceDef::callables))
    }
}

/// The Cargo package that declared an item.
///
/// Bimetal uses this value to distinguish Rust items and derive their public
/// namespace inside one application.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CargoPackageId(
    /// Cargo package name used as the ownership identity.
    pub String,
);

impl CargoPackageId {
    /// Create a package identity from a Cargo package name.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for CargoPackageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A Rust item identity inside the application.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionId {
    /// Cargo package that owns the definition.
    pub owner: CargoPackageId,
    /// Stable Rust declaration identity, including its module path.
    pub id: String,
}

impl DefinitionId {
    /// Create a definition identity from its owner and Rust declaration path.
    #[must_use]
    pub fn new(owner: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            owner: CargoPackageId::new(owner),
            id: id.into(),
        }
    }
}

impl fmt::Display for DefinitionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}::{}", self.owner, self.id)
    }
}

/// A type that can cross the generated host boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TypeRef {
    /// Rust's unit type, represented as null by host languages.
    Unit,
    /// A Boolean value.
    Bool,
    /// A fixed-width signed or unsigned integer.
    Int {
        /// Whether the integer is signed.
        signed: bool,
        /// Width in bits.
        bits: u16,
    },
    /// An IEEE-754 floating-point value.
    Float {
        /// Width in bits.
        bits: u16,
    },
    /// A UTF-8 string.
    String,
    /// An RFC 3339 date-time value.
    DateTime,
    /// A calendar date, which Serde writes as `YYYY-MM-DD`.
    Date,
    /// A span of time, which Serde writes as `{"secs": ..., "nanos": ...}` and the hosts read
    /// as a count of nanoseconds.
    Duration,
    /// An arbitrary JSON value.
    Json,
    /// A nullable value.
    Option {
        /// Inner non-null value type.
        item: Box<TypeRef>,
    },
    /// A variable-length sequence.
    List {
        /// Element type.
        item: Box<TypeRef>,
    },
    /// A string-keyed map.
    Map {
        /// Mapped value type.
        value: Box<TypeRef>,
    },
    /// A fixed-length heterogeneous sequence.
    Tuple {
        /// Element types in positional order.
        items: Vec<TypeRef>,
    },
    /// A reference to a linked model declaration.
    Named {
        /// Identity of the referenced declaration.
        identity: DefinitionId,
    },
    /// An owned or borrowed byte sequence using the direct binary ABI.
    Bytes,
    /// A fixed-length byte sequence using the direct binary ABI.
    FixedBytes {
        /// Required byte length.
        length: u64,
    },
    /// A contiguous numeric sequence using the direct typed-buffer ABI.
    Buffer {
        /// Primitive buffer element type.
        element: BufferElement,
    },
    /// An Apache Arrow value using the Arrow C Data Interface in Python and an
    /// Arrow IPC stream in Wasm.
    Arrow {
        /// Which Arrow value crosses the boundary.
        value: ArrowKind,
    },
    /// A resource, which crosses as its host object.
    Resource {
        /// Identity of the resource declaration.
        identity: DefinitionId,
    },
    /// A host function that Rust calls.
    Callback {
        /// The types of the parameters, then the type of the return.
        signature: Vec<TypeRef>,
    },
}

impl TypeRef {
    /// Return the references directly inside this one: the item of an option
    /// or a list, the value of a map, or the items of a tuple.
    #[must_use]
    pub fn children(&self) -> &[TypeRef] {
        match self {
            Self::Option { item } | Self::List { item } | Self::Map { value: item } => {
                slice::from_ref(item)
            }
            Self::Tuple { items } => items,
            Self::Callback { signature } => signature,
            _ => &[],
        }
    }
}

/// The Arrow values that can cross a host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArrowKind {
    /// One array: a pyarrow `Array` or an apache-arrow `Vector`.
    Array,
    /// One record batch.
    RecordBatch,
    /// A schema and any number of record batches: a `Table` in Python and
    /// TypeScript.
    Table,
    /// A schema without data.
    Schema,
    /// A schema and record batches that the reader pulls one at a time: a
    /// `RecordBatchReader` in Python. TypeScript has no equivalent yet.
    Stream,
}

/// A supported element type for a contiguous numeric buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BufferElement {
    /// Unsigned 8-bit integer.
    U8,
    /// Signed 8-bit integer.
    I8,
    /// Unsigned 16-bit integer.
    U16,
    /// Signed 16-bit integer.
    I16,
    /// Unsigned 32-bit integer.
    U32,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 64-bit integer.
    U64,
    /// Signed 64-bit integer.
    I64,
    /// 32-bit floating-point value.
    F32,
    /// 64-bit floating-point value.
    F64,
}

impl BufferElement {
    /// Every element type, in the order of the variants.
    pub const ALL: [Self; 10] = [
        Self::U8,
        Self::I8,
        Self::U16,
        Self::I16,
        Self::U32,
        Self::I32,
        Self::U64,
        Self::I64,
        Self::F32,
        Self::F64,
    ];

    /// Return the element type whose Rust type is `name`, such as `f64`.
    #[must_use]
    pub fn from_rust_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|element| element.rust_name() == name)
    }

    /// Return the Rust type, such as `f64`.
    #[must_use]
    pub const fn rust_name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }

    /// Return NumPy's name of the type, such as `float64`, which the schemas
    /// of both generated runtimes also use.
    #[must_use]
    pub const fn numpy_name(self) -> &'static str {
        match self {
            Self::U8 => "uint8",
            Self::I8 => "int8",
            Self::U16 => "uint16",
            Self::I16 => "int16",
            Self::U32 => "uint32",
            Self::I32 => "int32",
            Self::U64 => "uint64",
            Self::I64 => "int64",
            Self::F32 => "float32",
            Self::F64 => "float64",
        }
    }

    /// Return the JavaScript typed array of the type, such as
    /// `Float64Array`.
    #[must_use]
    pub const fn typed_array(self) -> &'static str {
        match self {
            Self::U8 => "Uint8Array",
            Self::I8 => "Int8Array",
            Self::U16 => "Uint16Array",
            Self::I16 => "Int16Array",
            Self::U32 => "Uint32Array",
            Self::I32 => "Int32Array",
            Self::U64 => "BigUint64Array",
            Self::I64 => "BigInt64Array",
            Self::F32 => "Float32Array",
            Self::F64 => "Float64Array",
        }
    }
}

/// A named Rust type in the application contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeDef {
    /// Cargo package that owns the type.
    pub owner: CargoPackageId,
    /// Rust module where the type is declared.
    pub rust_module: String,
    /// Stable Rust declaration identity.
    pub id: String,
    /// Public host-language type name.
    pub name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// Serializable host-language representation.
    pub shape: TypeShape,
}

impl TypeDef {
    /// Return this type's globally unique definition identity.
    #[must_use]
    pub fn identity(&self) -> DefinitionId {
        DefinitionId::new(self.owner.0.clone(), self.id.clone())
    }

    /// Return the types of the fields, of what external variants carry, or
    /// the target of an alias.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        let (fields, variants, external, target): (
            &[FieldDef],
            &[EnumVariantDef],
            &[ExternalVariantDef],
            _,
        ) = match &self.shape {
            TypeShape::Struct { fields } => (fields, &[], &[], None),
            TypeShape::StringEnum { variants } | TypeShape::TaggedEnum { variants, .. } => {
                (&[], variants, &[], None)
            }
            TypeShape::ExternalEnum { variants } => (&[], &[], variants, None),
            TypeShape::Alias { target } => (&[], &[], &[], Some(target)),
        };
        fields
            .iter()
            .chain(variants.iter().flat_map(|variant| &variant.fields))
            .map(|field| &field.ty)
            .chain(external.iter().flat_map(ExternalVariantDef::type_refs))
            .chain(target)
    }
}

/// The host-visible shape of a named Rust type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TypeShape {
    /// An object with named fields.
    Struct {
        /// Fields in Rust declaration order.
        fields: Vec<FieldDef>,
    },
    /// An enum represented by string literals.
    StringEnum {
        /// Variants in Rust declaration order.
        variants: Vec<EnumVariantDef>,
    },
    /// A discriminated union represented by tagged objects.
    TaggedEnum {
        /// Serialized discriminator field name.
        tag: String,
        /// Variants in Rust declaration order.
        variants: Vec<EnumVariantDef>,
    },
    /// An enum in Serde's default representation: a variant without data is
    /// its name, and a variant with data is an object whose only key is its
    /// name.
    ExternalEnum {
        /// Variants in Rust declaration order.
        variants: Vec<ExternalVariantDef>,
    },
    /// A transparent named alias around another contract type.
    Alias {
        /// Host representation wrapped by the alias.
        target: TypeRef,
    },
}

/// A field in a struct or tagged-enum variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldDef {
    /// Original Rust field name.
    pub rust_name: String,
    /// Serialized and host-visible field name.
    pub wire_name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// Host-neutral field type.
    pub ty: TypeRef,
    /// Whether callers must supply the field.
    pub required: bool,
    /// What an omitted field holds: the Rust `Default` of its type, as Serde
    /// writes it.
    pub default: Option<Value>,
    /// Host-side validation constraints.
    pub constraints: FieldConstraints,
}

/// A scalar value supported by field constraints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    /// Boolean scalar.
    Bool(bool),
    /// Signed integer scalar.
    I64(i64),
    /// UTF-8 string scalar.
    String(String),
}

/// Validation constraints copied to generated host models.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldConstraints {
    /// Exact literal accepted by the field.
    pub literal: Option<ScalarValue>,
    /// Inclusive minimum string or collection length.
    pub min_length: Option<u64>,
    /// Inclusive maximum string or collection length.
    pub max_length: Option<u64>,
    /// Inclusive numeric lower bound.
    pub ge: Option<i64>,
    /// Inclusive numeric upper bound.
    pub le: Option<i64>,
}

/// A variant in a string or tagged enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnumVariantDef {
    /// Original Rust variant name.
    pub rust_name: String,
    /// Serialized variant name.
    pub wire_name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// Fields carried by a tagged variant; empty for string enums.
    pub fields: Vec<FieldDef>,
}

/// A variant of an externally tagged enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalVariantDef {
    /// Original Rust variant name.
    pub rust_name: String,
    /// Serialized variant name, and the key its data crosses under.
    pub wire_name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// What the variant carries.
    pub payload: VariantPayload,
}

impl ExternalVariantDef {
    /// Return the types that the variant carries.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        let (value, fields): (Option<&TypeRef>, &[FieldDef]) = match &self.payload {
            VariantPayload::Unit => (None, &[]),
            VariantPayload::Value { ty } => (Some(ty), &[]),
            VariantPayload::Fields { fields } => (None, fields),
        };
        value
            .into_iter()
            .chain(fields.iter().map(|field| &field.ty))
    }
}

/// What a variant of an externally tagged enum carries under its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum VariantPayload {
    /// Nothing: the variant crosses as its name alone.
    Unit,
    /// The one value of a one-field tuple variant.
    Value {
        /// The value's type.
        ty: TypeRef,
    },
    /// The named fields of a struct variant, which cross as an object.
    Fields {
        /// Fields in Rust declaration order.
        fields: Vec<FieldDef>,
    },
}

/// A named Rust error in the application contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorDef {
    /// Cargo package that owns the error.
    pub owner: CargoPackageId,
    /// Rust module where the error is declared.
    pub rust_module: String,
    /// Stable Rust declaration identity.
    pub id: String,
    /// Public host-language error class name.
    pub name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// Every code the error can report, in declaration order.
    pub codes: Vec<ErrorCode>,
}

/// One stable code that an error can report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorCode {
    /// The code that hosts receive.
    pub code: String,
    /// Normalized Rust documentation of the variant, when present.
    pub docs: Option<String>,
    /// The model of the fields that an error with this code carries, when it
    /// declares `#[bimetal(details)]`.
    pub details: Option<DefinitionId>,
}

impl ErrorDef {
    /// Return this error's globally unique definition identity.
    #[must_use]
    pub fn identity(&self) -> DefinitionId {
        DefinitionId::new(self.owner.0.clone(), self.id.clone())
    }
}

/// What hosts call: an exported function, resource method, or resource
/// constructor, without what it returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallableDef {
    /// Original Rust name.
    pub rust_name: String,
    /// Public host-language name.
    pub host_name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// Parameters in declaration order, after any receiver.
    pub params: Vec<ParamDef>,
    /// Typed error returned by the callable, when any.
    pub error: Option<DefinitionId>,
    /// Whether the callable is an `async fn`, which hosts await.
    pub is_async: bool,
    /// Whether `#[bimetal(brief)]` marks a synchronous call: Python keeps its
    /// interpreter lock through it, and binds the native function in place of
    /// a generated function that would only pass its arguments on.
    pub brief: bool,
}

impl CallableDef {
    /// Return the types of the parameters.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        self.params.iter().map(|param| &param.ty)
    }
}

/// What an export is, which its native name records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    /// A free function.
    Function,
    /// A resource.
    Resource,
}

impl ExportKind {
    /// Return the kind as a native name spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Resource => "resource",
        }
    }

    /// Return what every native name of this kind starts with:
    /// `__bimetal_<kind>_`, which the package's name and a hash follow.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Function => "__bimetal_function_",
            Self::Resource => "__bimetal_resource_",
        }
    }
}

/// A free function exposed to the hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionDef {
    /// Cargo package that owns the function.
    pub owner: CargoPackageId,
    /// Rust module where the function is declared.
    pub rust_module: String,
    /// The globally unique name of the native target.
    pub native_name: String,
    /// The function's names, documentation, parameters, and error.
    pub callable: CallableDef,
    /// Successful return type.
    pub returns: TypeRef,
}

impl FunctionDef {
    /// Return the types of the parameters, then the return type.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        self.callable.type_refs().chain(iter::once(&self.returns))
    }
}

/// A parameter in an exported function, constructor, or method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParamDef {
    /// Original Rust parameter name.
    pub rust_name: String,
    /// Public host-language parameter name.
    pub host_name: String,
    /// Host-neutral parameter type.
    pub ty: TypeRef,
    /// What a host passes when the caller omits the argument.
    pub default: Option<Value>,
}

/// An exported Rust type that keeps native state between calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceDef {
    /// Cargo package that owns the resource.
    pub owner: CargoPackageId,
    /// Rust module where the resource is declared.
    pub rust_module: String,
    /// Stable Rust declaration identity.
    pub id: String,
    /// Public host-language resource class name.
    pub name: String,
    /// The globally unique name of the native class.
    pub native_name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// The class constructor: the synchronous `new`, or otherwise the first
    /// synchronous constructor. Host constructors cannot be async, so a
    /// resource whose constructors are all async has none, and neither has
    /// one that only other exports return. Every constructor returns the new
    /// resource.
    pub constructor: Option<CallableDef>,
    /// Every other constructor, exposed as a named factory.
    pub factories: Vec<CallableDef>,
    /// Public instance methods.
    pub methods: Vec<MethodDef>,
}

impl ResourceDef {
    /// The member of every resource class, on every host, that releases its
    /// value.
    pub const CLOSE: &'static str = "close";

    /// The member of every resource class, on every host, that says whether
    /// its value was released.
    pub const CLOSED: &'static str = "closed";

    /// Return this resource's globally unique definition identity.
    #[must_use]
    pub fn identity(&self) -> DefinitionId {
        DefinitionId::new(self.owner.0.clone(), self.id.clone())
    }

    /// Return the method that implements `protocol`, when any.
    #[must_use]
    pub fn protocol_method(&self, protocol: Protocol) -> Option<&MethodDef> {
        self.methods
            .iter()
            .find(|method| method.protocol == Some(protocol))
    }

    /// Return the class constructor, when any, followed by the factories.
    pub fn constructors(&self) -> impl Iterator<Item = &CallableDef> {
        self.constructor.iter().chain(&self.factories)
    }

    /// Return the constructors, then the methods.
    pub fn callables(&self) -> impl Iterator<Item = &CallableDef> {
        self.constructors()
            .chain(self.methods.iter().map(|method| &method.callable))
    }

    /// Return the types of the constructors' parameters, then those of each
    /// method's parameters and return.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        self.constructors()
            .flat_map(CallableDef::type_refs)
            .chain(self.methods.iter().flat_map(MethodDef::type_refs))
    }
}

/// A callable method on an exported resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MethodDef {
    /// The method's names, documentation, parameters, and error.
    pub callable: CallableDef,
    /// Successful return type.
    pub returns: TypeRef,
    /// The host language feature that the method also implements, when any.
    pub protocol: Option<Protocol>,
}

/// A host language feature that a resource method implements, beyond being a
/// method that callers can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    /// Runs when a `with` block starts.
    Enter,
    /// Runs when a `with` block or a `using` declaration ends.
    Exit,
    /// Gives `for` and `for of` their next value, and ends them with `None`.
    Next,
    /// A property that callers read without calling.
    Getter,
    /// A property that callers assign.
    Setter,
    /// Python's `len(value)`, and a `length` property in JavaScript.
    Len,
    /// Python's `repr(value)`, and what Node.js prints for the value.
    Repr,
    /// Python's `str(value)`, and JavaScript's `toString()`.
    Display,
    /// Python's `value[key]`.
    GetItem,
    /// Python's `value[key] = item`.
    SetItem,
    /// Python's `del value[key]`.
    DelItem,
    /// Python's `item in value`.
    Contains,
    /// Python's `value == other`.
    Eq,
    /// Python's `hash(value)`.
    Hash,
    /// Python's `bool(value)`.
    Bool,
    /// Python's `value(...)`.
    Call,
}

impl Protocol {
    /// Every protocol.
    pub const ALL: [Self; 16] = [
        Self::Enter,
        Self::Exit,
        Self::Next,
        Self::Getter,
        Self::Setter,
        Self::Len,
        Self::Repr,
        Self::Display,
        Self::GetItem,
        Self::SetItem,
        Self::DelItem,
        Self::Contains,
        Self::Eq,
        Self::Hash,
        Self::Bool,
        Self::Call,
    ];

    /// Return the attribute that declares the protocol, such as `len` in
    /// `#[bimetal(len)]`.
    #[must_use]
    pub const fn attribute(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Exit => "exit",
            Self::Next => "next",
            Self::Getter => "getter",
            Self::Setter => "setter",
            Self::Len => "len",
            Self::Repr => "repr",
            Self::Display => "display",
            Self::GetItem => "getitem",
            Self::SetItem => "setitem",
            Self::DelItem => "delitem",
            Self::Contains => "contains",
            Self::Eq => "eq",
            Self::Hash => "hash",
            Self::Bool => "bool",
            Self::Call => "call",
        }
    }

    /// Return the variant's name in `bimetal::ir::Protocol`, for generated code.
    #[must_use]
    pub const fn variant(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::Exit => "Exit",
            Self::Next => "Next",
            Self::Getter => "Getter",
            Self::Setter => "Setter",
            Self::Len => "Len",
            Self::Repr => "Repr",
            Self::Display => "Display",
            Self::GetItem => "GetItem",
            Self::SetItem => "SetItem",
            Self::DelItem => "DelItem",
            Self::Contains => "Contains",
            Self::Eq => "Eq",
            Self::Hash => "Hash",
            Self::Bool => "Bool",
            Self::Call => "Call",
        }
    }
}

impl MethodDef {
    /// Return the types of the parameters, then the return type.
    pub fn type_refs(&self) -> impl Iterator<Item = &TypeRef> {
        self.callable.type_refs().chain(iter::once(&self.returns))
    }
}

/// A Rust constant exposed to the hosts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConstantDef {
    /// Cargo package that owns the constant.
    pub owner: CargoPackageId,
    /// Rust module where the constant is declared.
    pub rust_module: String,
    /// Public host-language constant name.
    pub host_name: String,
    /// Normalized Rust documentation, when present.
    pub docs: Option<String>,
    /// Host-neutral constant type.
    pub ty: TypeRef,
    /// Serialized constant value.
    pub value: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_name_prefixes_spell_their_kind() {
        for kind in [ExportKind::Function, ExportKind::Resource] {
            assert_eq!(kind.prefix(), format!("__bimetal_{}_", kind.name()));
        }
    }

    #[test]
    fn buffer_elements_serialize_as_their_rust_names() {
        for element in BufferElement::ALL {
            assert_eq!(
                serde_json::to_value(element).expect("an element serializes"),
                element.rust_name()
            );
        }
    }
}
