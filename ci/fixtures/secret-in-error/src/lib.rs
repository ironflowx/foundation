//! Violates TEN-3: each `Bad` type is an error that can hold a `Secret`. No `Fine` one is.

use std::error::Error;
use std::fmt;

use ironflowx_core::Secret;
use ironflowx_core::Secret as Renamed;
use ironflowx_proto::Call;

macro_rules! error {
    ($($name:ty),*) => { $(
        impl fmt::Debug for $name {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result { Ok(()) }
        }
        impl fmt::Display for $name {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result { Ok(()) }
        }
        impl Error for $name {}
    )* };
}

pub type Alias = Option<Secret>;
pub type Pair<T> = (T, Renamed);
type Chain1 = Alias;
type Chain2 = Chain1;
struct Nested {
    _chain: Vec<Chain2>,
}
mod hidden {
    pub struct Deep {
        pub(super) _nested: Box<super::Nested>,
    }
}
pub trait Has {
    type Held;
}
impl Has for u8 {
    type Held = Secret;
}
macro_rules! field {
    ($name:ident) => {
        pub struct $name {
            _key: Secret,
        }
    };
}

pub struct BadField {
    pub key: Secret,
}
pub struct BadPrivate(#[allow(dead_code)] Renamed);
/// A `Call` prints itself, so a derive compiles.
#[derive(Debug)]
pub struct BadCall {
    _request: Call,
}
impl fmt::Display for BadCall {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}
impl Error for BadCall {}
pub enum BadVariant {
    Fine,
    Key { key: Alias },
}
pub enum BadTuple {
    Key(u8, Pair<u8>),
}
pub struct BadDeep(#[allow(dead_code)] hidden::Deep);
pub struct BadAssociated(pub <u8 as Has>::Held);
pub struct BadWrapper<T>(pub T);
pub union BadUnion {
    pub key: std::mem::ManuallyDrop<Secret>,
}
field!(BadMacro);
pub struct NamedError(pub Secret);
pub struct NamedRefused(#[allow(dead_code)] Call);

pub struct SecretName(pub String);
pub struct FinePointer(pub fn(&Secret) -> bool);
pub struct FineName(pub SecretName);
pub struct FineHolder(pub Secret);

error!(
    BadField, BadPrivate, BadVariant, BadTuple, BadDeep, BadAssociated, BadWrapper<Secret>,
    BadUnion, BadMacro, FinePointer, FineName
);
