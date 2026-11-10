//! A SQL function that calls back into the host.

use std::sync::Arc;

use bimetal::Callback;
use datafusion::arrow::array::{ArrayRef, Float64Array};
use datafusion::arrow::datatypes::DataType;
use datafusion::error::{DataFusionError, Result};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};

/// Make a SQL function named `name` that runs `function` once per row.
pub fn udf(name: &str, function: Callback<(f64, f64), f64>) -> ScalarUDF {
    ScalarUDF::new_from_impl(HostFunction {
        name: name.to_owned(),
        signature: Signature::exact(
            vec![DataType::Float64, DataType::Float64],
            Volatility::Volatile,
        ),
        function,
    })
}

#[derive(Debug)]
struct HostFunction {
    name: String,
    signature: Signature,
    function: Callback<(f64, f64), f64>,
}

impl std::hash::Hash for HostFunction {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl PartialEq for HostFunction {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for HostFunction {}

impl ScalarUDFImpl for HostFunction {
    fn name(&self) -> &str {
        &self.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> Result<DataType> {
        Ok(DataType::Float64)
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let arrays = ColumnarValue::values_to_arrays(&args.args)?;
        let left = arrays[0].as_any().downcast_ref::<Float64Array>();
        let right = arrays[1].as_any().downcast_ref::<Float64Array>();
        let (Some(left), Some(right)) = (left, right) else {
            return Err(DataFusionError::Internal(
                "expected float arguments".to_owned(),
            ));
        };
        let mut values = Vec::with_capacity(left.len());
        for (left, right) in left.iter().zip(right) {
            values.push(match (left, right) {
                (Some(left), Some(right)) => Some(
                    self.function
                        .call((left, right))
                        .map_err(|error| DataFusionError::External(Box::new(error)))?,
                ),
                _ => None,
            });
        }
        let array: ArrayRef = Arc::new(Float64Array::from(values));
        Ok(ColumnarValue::Array(array))
    }
}
