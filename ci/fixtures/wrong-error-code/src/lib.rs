//! A doctest that fails to compile, but not with the code it names, which stable does not check:
//!
//! ```compile_fail,E0599
//! let n: u32 = "E0308";
//! ```
//!
//! And one that names none:
//!
//! ```compile_fail
//! let n: u32 = "E0308";
//! ```
