//! Protocol session state is owned by this plugin.
pub mod remote;
pub struct BusinessHub {
    pub remote: remote::RemoteHub,
}
impl BusinessHub {
    pub fn new() -> Self {
        Self {
            remote: Default::default(),
        }
    }
}
