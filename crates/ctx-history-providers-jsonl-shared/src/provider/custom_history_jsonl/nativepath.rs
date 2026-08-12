mod reader;
mod source_backed;

pub(crate) use source_backed::{
    CustomHistorySourceBackedInput, custom_history_jsonl_family_adapter,
};

#[cfg(all(test, feature = "capture-integration-tests"))]
mod source_backed_tests;
