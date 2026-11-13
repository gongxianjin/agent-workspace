//! Accepted UI request correlations, never task/domain state.
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Pending {
    requests: BTreeMap<u64, String>,
}
impl Pending {
    pub fn accept(&mut self, action: String, request_id: u64) {
        self.requests.insert(request_id, action);
    }
    pub fn contains(&self, action: &str) -> bool {
        self.requests.values().any(|pending| pending == action)
    }
    pub fn complete(&mut self, request_id: u64) -> Option<String> {
        self.requests.remove(&request_id)
    }
    pub fn len(&self) -> usize { self.requests.len() }
}
