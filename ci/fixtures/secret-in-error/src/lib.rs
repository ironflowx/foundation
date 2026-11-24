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
pub type Chain1 = Alias;
pub type Chain2 = Chain1;
pub type Chain3 = Chain2;
pub type Chain4 = Chain3;
pub type Chain5 = Chain4;
pub type Chain6 = Chain5;
pub type Chain7 = Chain6;
pub type Chain8 = Chain7;
pub type Chain9 = Chain8;
pub type Chain10 = Chain9;
pub type Chain11 = Chain10;
pub type Chain12 = Chain11;
struct Nested {
    _chain: Vec<Chain12>,
}
pub struct Holder<T = Secret>(pub T);
pub type DefaultAlias<T = Secret> = Option<T>;
#[doc(hidden)]
pub struct HiddenHolder(pub Secret);
#[doc(hidden)]
pub mod hidden_module {
    pub struct Inner(pub super::Secret);
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
pub struct BadHiddenField {
    #[doc(hidden)]
    pub key: Secret,
}
pub enum BadHiddenVariant {
    #[doc(hidden)]
    Key(Secret),
}
pub struct BadHiddenType(pub HiddenHolder);
pub struct BadHiddenModule(pub hidden_module::Inner);
pub struct BadDefault<T = Secret>(pub T);
pub struct BadDefaultHolder(pub Holder);
pub struct BadDefaultAlias(pub DefaultAlias);
/// Generic: it holds what it is used with. A `Call` prints itself, so a derive compiles.
#[derive(Debug)]
pub struct BadGeneric<T>(pub T);
impl<T> fmt::Display for BadGeneric<T> {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}
impl<T: fmt::Debug> Error for BadGeneric<T> {}
pub fn bad_generic() -> Result<(), BadGeneric<Call>> {
    Ok(())
}
pub struct BadAliasedError<T>(pub T);
pub type AliasedResult<T> = Result<(), BadAliasedError<T>>;
pub type AliasedAgain<T> = AliasedResult<Option<T>>;
pub fn bad_aliased() -> AliasedAgain<Secret> {
    Ok(())
}
// Named as errors, each reached by one way only.
pub struct BadDefaultedError<T>(pub T);
pub type Defaulted<T, E = BadDefaultedError<T>> = Result<T, E>;
pub fn bad_defaulted() -> Defaulted<Secret> {
    Err(BadDefaultedError(Secret::new(String::new())))
}
pub struct BadEnvelopeError<T>(pub T);
pub struct Envelope<T> {
    _outcome: Result<(), BadEnvelopeError<T>>,
}
pub enum Outer<T> {
    Reply(Option<Envelope<T>>),
}
pub fn bad_envelope() -> Outer<Call> {
    Outer::Reply(None)
}
pub struct BadMethodError<T>(pub T);
pub struct Client<T>(pub Option<T>);
impl<R> Client<R> {
    pub fn send(&self) -> Result<(), BadMethodError<R>> {
        Ok(())
    }
}
pub fn bad_method() -> Client<Call> {
    Client(None)
}
pub struct BadMixedError<T>(pub T);
pub struct Keyed<K, T>(pub K, pub T);
impl<R> Keyed<u8, R> {
    pub fn send(&self) -> Result<(), BadMixedError<R>> {
        Ok(())
    }
}
pub fn bad_mixed(_: Keyed<u8, Call>) {}
pub struct BadSelfError<T>(pub T);
pub struct Carrier(pub Secret);
impl Carrier {
    pub fn retry(self) -> Result<(), BadSelfError<Self>> {
        Ok(())
    }
}
pub struct BadSelfForeignError<T>(pub T);
pub trait Retry: Sized {
    fn retry(self) -> Result<(), BadSelfForeignError<Self>>;
}
impl Retry for Option<Secret> {
    fn retry(self) -> Result<(), BadSelfForeignError<Self>> {
        Ok(())
    }
}
pub struct BadSelfMethodError<T>(pub T);
impl<R> Client<R> {
    pub fn again(self) -> Result<(), BadSelfMethodError<Self>> {
        Ok(())
    }
}
pub struct BadSelfPlainError<T>(pub T);
pub struct SelfPlain {
    _key: Secret,
    _last: Option<Box<BadSelfPlainError<Self>>>,
}
pub struct BadSelfFieldError<T>(pub T);
pub struct SelfField<T> {
    _key: Option<T>,
    _last: Option<Box<BadSelfFieldError<Self>>>,
}
pub fn bad_self_field() -> SelfField<Secret> {
    SelfField { _key: None, _last: None }
}
pub fn bad_dependency() -> Result<(), std::sync::mpsc::SendError<Secret>> {
    Ok(())
}
pub struct NamedGenericError<T>(pub T);
pub struct UsesNamed {
    _last: Option<NamedGenericError<Alias>>,
}
pub struct NamedError(pub Secret);
pub struct NamedRefused(#[allow(dead_code)] Call);
pub struct NamedMalformed(pub Secret);

pub struct SecretName(pub String);
pub struct FinePointer(pub fn(&Secret) -> bool);
pub struct FineName(pub SecretName);
pub struct FineHolder(pub Secret);
/// Generic, used with no `Secret`: not by a result that holds one when all is well.
#[derive(Debug)]
pub struct FineGeneric<T>(pub T);
impl<T> fmt::Display for FineGeneric<T> {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}
impl<T: fmt::Debug> Error for FineGeneric<T> {}
pub type FineResult<T> = Result<T, FineGeneric<u8>>;
pub fn fine_generic() -> FineResult<Secret> {
    Err(FineGeneric(0))
}
pub struct FineParamError<T>(pub T);
pub type FineSplit<A, B> = Result<A, FineParamError<B>>;
pub fn fine_split() -> FineSplit<Secret, u8> {
    Err(FineParamError(0))
}
pub type FineCallback<T> = FineParamError<fn(T)>;
pub fn fine_callback(_: FineCallback<Secret>) {}
pub struct FineNode<T> {
    _own: Option<T>,
    _next: Option<Box<FineNode<Option<T>>>>,
}
pub fn fine_node(_: FineNode<FineParamError<u8>>) {}
pub fn fine_pointer() -> Result<(), FineGeneric<fn(&Secret) -> bool>> {
    Ok(())
}

error!(
    BadField, BadPrivate, BadVariant, BadTuple, BadDeep, BadAssociated, BadWrapper<Secret>,
    BadUnion, BadMacro, BadHiddenField, BadHiddenVariant, BadHiddenType, BadHiddenModule,
    BadDefault, BadDefaultHolder, BadDefaultAlias, FinePointer, FineName
);
