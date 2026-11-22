// Not compiled: `Self::key` from a second impl block in another file of ironflowx-core.
impl crate::SubmitAs {
    pub fn again(id: crate::KeyId) -> Self {
        Self::key(id)
    }
}
