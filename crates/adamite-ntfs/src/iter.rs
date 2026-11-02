//! Shared completion handling for fallible, fused parser iterators.

pub(crate) fn finish_step<T, E>(
    step: Result<Option<T>, E>,
    finished: &mut bool,
) -> Option<Result<T, E>> {
    if !matches!(&step, Ok(Some(_))) {
        *finished = true;
    }
    step.transpose()
}
