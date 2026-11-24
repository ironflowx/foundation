//! A doctest that fails to compile, but not with the code it names, which stable does not check:
//!
//! ```compile_fail,E0599
//! let n: u32 = "E0308";
//! ```
//!
//! And some that name none: rustdoc takes only an `E` and four digits as a code.
//!
//! ```compile_fail
//! let n: u32 = "E0308";
//! ```
//!
//! ~~~compile_fail,E03080
//! let n: u32 = "E0308";
//! ~~~
//!
//! ```compile_fail,fooE0308
//! let n: u32 = "E0308";
//! ```
//!
//! ```compile_fail (E0308)
//! let n: u32 = "E0308";
//! ```
//!
//! ```compile_fail,E0308.
//! let n: u32 = "E0308";
//! ```
#![doc = include_str!("included.md")]
