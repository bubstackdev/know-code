use know_code_core::admission::AdmissionScope;
use std::path::Path;

pub fn scope(database: impl AsRef<Path>) -> AdmissionScope {
    AdmissionScope::at(database.as_ref().parent().unwrap().join("query-admission")).unwrap()
}
