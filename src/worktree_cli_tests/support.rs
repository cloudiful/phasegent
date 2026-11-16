pub(crate) fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}
