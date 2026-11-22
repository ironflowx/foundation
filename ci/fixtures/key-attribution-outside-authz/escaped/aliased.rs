// Not compiled: the forms only ci/no-key-attribution.sh sees inside ironflowx-core, where
// clippy cannot resolve the path. A renamed import:
use crate::SubmitAs as S;

pub fn aliased(id: crate::KeyId) -> S {
    S::key(id)
}
