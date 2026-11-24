//! A whole key's text, which must never be printed, copied or left in memory.

use zeroize::Zeroizing;

/// A whole key's text, as presented or minted. `expose()` is its only read, and it is zeroed on
/// drop. It implements none of `Debug`, `Display`, `Serialize` and `Clone`, so it cannot reach a
/// log or a copy by accident (TEN-3):
///
/// ```compile_fail,E0277
/// # use ironflowx_core::Secret;
/// fn print(s: &Secret) -> String { format!("{s:?}") }
/// ```
///
/// ```compile_fail,E0277
/// # use ironflowx_core::Secret;
/// fn print(s: &Secret) -> String { format!("{s}") }
/// ```
///
/// ```compile_fail,E0599
/// # use ironflowx_core::Secret;
/// fn copy(s: Secret) -> (Secret, Secret) { (s.clone(), s) }
/// ```
///
/// ```compile_fail,E0277
/// # use ironflowx_core::Secret;
/// fn serializable<T: serde_core::Serialize>() {}
/// serializable::<Secret>();
/// ```
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}
