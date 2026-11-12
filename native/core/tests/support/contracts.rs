//! What the native host's contracts gave for the inputs a test's rules ask, as a fixture recorded
//! them (Issue #225, lane M7).
//!
//! A pydantic contract is Python's (`Host::contract`): the native host validates the input with the
//! model and gives its dump (`model_dump(mode="json")`), or raises what `model_validate` raised. A
//! fixture records that for each input a test needs: `{"model": name, "op": op, "input": value,
//! "dump": value}` or `{..., "raises": [class, text]}`, the class `Value` or `Type`, `op` only for
//! `_PARAMETER_MODELS` (the model of an operator's parameters). `recorded` is the
//! `TestHost.contracts` that answers so; any other input is given back, as the `TestHost` does.

use serde_json::Value;
use sheltersim_core::error::Error;
use sheltersim_core::host::Contract;
use sheltersim_core::value::V;

use super::load;

/// One recorded answer.
struct Asked {
    model: String,
    op: Option<String>,
    input: V,
    answer: Result<V, (bool, String)>,
}

/// The contracts of a fixture's list of answers.
pub fn recorded(asked: &Value) -> Box<dyn FnMut(&Contract, &V) -> Result<V, Error>> {
    let asked: Vec<Asked> = asked
        .as_array()
        .unwrap_or_else(|| panic!("not a list of contracts: {asked}"))
        .iter()
        .map(|item| Asked {
            model: item["model"].as_str().unwrap().to_owned(),
            op: item.get("op").map(|op| op.as_str().unwrap().to_owned()),
            input: load::v(&item["input"]),
            answer: match item.get("dump") {
                Some(dump) => Ok(load::v(dump)),
                None => Err((item["raises"][0] == "Type", item["raises"][1].as_str().unwrap().to_owned())),
            },
        })
        .collect();
    Box::new(move |which, input| {
        let op = match which {
            Contract::EffectParameters(op) => Some(&**op),
            _ => None,
        };
        let found = asked.iter().find(|item| item.model == which.model().1 && item.op.as_deref() == op && item.input == *input);
        match found {
            None => Ok(input.clone()),
            Some(item) => match &item.answer {
                Ok(dump) => Ok(dump.clone()),
                Err((true, text)) => Err(Error::Type(text.clone())),
                Err((false, text)) => Err(Error::Value(text.clone())),
            },
        }
    })
}
