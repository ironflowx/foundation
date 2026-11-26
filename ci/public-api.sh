#!/usr/bin/env bash
# Usage: ci/public-api.sh [--bless]
# Compares each foundation crate's public API with its snapshot in public-api/ (D21); any
# difference fails. --bless rewrites the snapshots after a reviewed change.
# Then checks LED-9, that no dependency type is public, and TEN-8 and TEN-4's API rules over the
# snapshots, TEN-3's over rustdoc's JSON, that each doctest that must not compile names an error
# code and fails with it, which stable does not check, and each check rejecting its fixture.
# Needs cargo-public-api and the nightly pinned in ci/tools.env (ci/install-tools.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=ci/tools.env
source ci/tools.env
rustup toolchain install "$NIGHTLY" --profile minimal --no-self-update >/dev/null
mkdir -p public-api
published=(ironflowx-core ironflowx-events ironflowx-proto ironflowx-cell ironflowx-authz
  ironflowx-store ironflowx-operations ironflowx)
for crate in "${published[@]}" ironflowx-behaviors ironflowx-bin ironflowx-cli \
  ironflowx-codegen ironflowx-crash-test ironflowx-doors; do
  # The proto client with the interface; not `wire` or `server`, which name buffa's and connectrpc's
  # types and follow their 0.x versions.
  features=()
  [ "$crate" != ironflowx-proto ] || features=(--features client)
  [ "$crate" != ironflowx-doors ] || features=(--features http)
  # -ss keeps derived impls, so a new derive (an Ord, a From) shows in the diff. Hidden items
  # are documented too, so a second #[doc(hidden)] constructor shows.
  RUSTDOCFLAGS="-Zunstable-options --document-hidden-items" cargo "+$NIGHTLY" public-api --color never -p "$crate" "${features[@]+"${features[@]}"}" -ss > "public-api/$crate.txt"
done
[ "${1:-}" = "--bless" ] && exit 0
# Untracked counts: a snapshot that was never added must fail too.
untracked=$(git ls-files --others --exclude-standard -- public-api/)
if ! git diff --quiet -- public-api/ || [ -n "$untracked" ]; then
  git diff -- public-api/
  echo "$untracked"
  echo "public API changed: review the diff, then run ci/public-api.sh --bless and commit it"
  exit 1
fi

ci/compile-fail-codes.sh crates docs README.md
ci/must-reject.sh wrong-error-code/src/lib.rs:9: wrong-error-code/src/lib.rs:13: \
  wrong-error-code/src/lib.rs:17: wrong-error-code/src/lib.rs:21: wrong-error-code/src/lib.rs:25: \
  wrong-error-code/src/included.md:1: -- \
  ci/compile-fail-codes.sh ci/fixtures/wrong-error-code
cargo "+$NIGHTLY" test -q --locked --doc --workspace
ci/must-reject.sh 'Some expected error codes were not found: ["E0599"]' -- \
  cargo "+$NIGHTLY" test -q --locked --doc --manifest-path ci/fixtures/wrong-error-code/Cargo.toml

ci/no-bare-env-id.sh public-api/*.txt
ci/no-dependency-types.sh public-api/*.txt
ci/api-rules.sh public-api/*.txt
ci/api-rules.sh ci/fixtures/api-rules-ok.txt
ci/no-bare-env-id.sh ci/fixtures/bare-environment-id-ok.txt
ci/must-reject.sh \
  'TEN-4: impl core::cmp::PartialEq for ironflowx_authz::KeyDigest' \
  'TEN-4: impl core::convert::AsRef<[u8]> for &ironflowx_authz::KeyDigest' \
  'TEN-4: impl core::convert::From<ironflowx_authz::KeyDigest> for [u8; 32]' \
  'TEN-4: pub fn ironflowx_authz::KeyDigest::as_bytes(&self) -> &[u8; 32]' \
  'TEN-4: pub fn ironflowx_authz::digest_bytes(&ironflowx_authz::KeyDigest) -> [u8; 32]' \
  'TEN-4: pub fn ironflowx_authz::digest_text(&ironflowx_authz::KeyDigest) -> ironflowx_authz::Text' \
  'TEN-4: pub fn ironflowx_authz::export((), &ironflowx_authz::KeyDigest) -> [u8; 32]' \
  'TEN-4: pub fn ironflowx_authz::export_with<' \
  'TEN-4: pub fn ironflowx_authz::export_where<' \
  'TEN-4: pub fn ironflowx_authz::exporter() -> fn(&ironflowx_authz::KeyDigest) -> [u8; 32]' \
  'TEN-4: pub ironflowx_authz::KeyDigest::0: [u8; 32]' \
  'TEN-4: pub struct ironflowx_authz::KeyDigest(pub [u8; 32])' \
  'TEN-4: pub type ironflowx_authz::Stored = core::option::Option<ironflowx_authz::KeyDigest>' \
  'TEN-8: impl core::convert::From<&ironflowx_core::tenancy::Address> for ironflowx_core::id::TenantId' \
  'TEN-8: impl core::convert::From<ironflowx_core::tenancy::Address> for ironflowx_core::id::ProjectId' \
  'TEN-8: impl core::convert::Into<ironflowx_core::id::ProjectId> for ironflowx_core::Address' \
  'TEN-8: impl core::convert::TryFrom<ironflowx_core::tenancy::Address> for ironflowx_core::id::ProjectId' \
  'TEN-8: pub fn ironflowx_core::Address::project_id(&self) -> ironflowx_core::id::ProjectId' \
  'TEN-8: pub fn ironflowx_core::ProjectId::from_address(&ironflowx_core::tenancy::Address) -> Self' \
  'TEN-8: pub fn ironflowx_store::ControlPlane::resolve(&self, &ironflowx_core::tenancy::Address) -> core::result::Result<ironflowx_store::Resolved, ironflowx_store::StoreError>' \
  'TEN-8: pub fn ironflowx_store::resolve((), &ironflowx_core::tenancy::Address) -> ironflowx_core::id::ProjectId' \
  'TEN-8: pub fn ironflowx_store::resolve_any<T>(T)' \
  'TEN-8: pub fn ironflowx_store::resolve_where<' \
  'TEN-8: pub ironflowx_store::Resolver::with:' \
  'TEN-8: pub type ironflowx_core::Place = ironflowx_core::tenancy::Address' \
  -- ci/api-rules.sh ci/fixtures/api-rules.txt
ci/must-reject.sh Ledger::open find_after ffi_open EnvironmentId::lookup \
  OpenRequest::env Target::Env proto::Wrapper store::LookupId cell::dispatch cell::any_id \
  cell::any_where cell::opener -- ci/no-bare-env-id.sh ci/fixtures/bare-environment-id.txt
ci/must-reject.sh rusqlite::error::Error segment_log_core::engine tokio::runtime uuid::Uuid \
  connectrpc::error::ConnectError buffa::error::DecodeError buffa_types::google http::uri::Uri \
  hyper::error::Error hyper_util::client serde_core::ser::Serialize bytes::bytes::Bytes \
  axum::routing::MethodRouter tower::util::BoxService clap_builder::error::Error \
  ironflowx_cli::matches clap_builder::builder::arg::Arg ironflowx_vendor::Handle \
  ironflowx_proto::ProbeError ironflowx_proto::probe_http connectrpc::client:: \
  ironflowx_proto::zz_http ironflowx::store -- \
  ci/no-dependency-types.sh ci/fixtures/dependency-type.txt

# TEN-3, over every item: rustdoc's JSON of each published crate, private and hidden items included,
# and every feature a build can switch on.
json() {
  cargo "+$NIGHTLY" rustdoc -q --locked "$@" -- -Zunstable-options --output-format json \
    --document-private-items --document-hidden-items --cap-lints allow
}
docs=()
for crate in "${published[@]}"; do
  json -p "$crate" --lib --all-features
  docs+=("target/doc/${crate//-/_}.json")
done
ci/no-secret-in-errors.sh "${docs[@]}"
bad=ci/fixtures/secret-in-error
json --manifest-path "$bad/Cargo.toml"
docs+=("$bad/target/doc/secret_in_error.json")
ci/must-reject.sh BadField BadPrivate BadCall BadVariant BadTuple BadDeep BadAssociated BadWrapper \
  BadUnion BadMacro BadHiddenField BadHiddenVariant BadHiddenType BadHiddenModule 'BadDefault is' \
  BadDefaultHolder BadDefaultAlias BadGeneric BadAliasedError BadDefaultedError \
  BadEnvelopeError BadMethodError BadMixedError BadDefaultMethodError \
  BadForeignMethodError BadPlacedError BadNamedAliasError BadSelfError BadSelfForeignError BadSelfMethodError \
  BadSelfPlainError BadSelfFieldError mpsc::SendError NamedGenericError \
  NamedError NamedRefused NamedMalformed -- ci/no-secret-in-errors.sh "${docs[@]}"
found=$(ci/no-secret-in-errors.sh "${docs[@]}" || true)
if grep Fine <<<"$found"; then
  echo "public-api: ci/no-secret-in-errors.sh rejects an error that holds no Secret"
  exit 1
fi
