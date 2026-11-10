//! How a generated name that the generated code reserves gets free.

/// Give each name for which `taken` holds trailing underscores, until
/// `taken` no longer holds and the name differs from the other names.
///
/// `taken` must fail for a name with enough trailing underscores, so that
/// the renaming ends.
pub(crate) fn disambiguate(names: &mut [String], taken: impl Fn(&str) -> bool) {
    for index in 0..names.len() {
        if !taken(&names[index]) {
            continue;
        }
        let mut name = names[index].clone();
        while taken(&name) || names.contains(&name) {
            name.push('_');
        }
        names[index] = name;
    }
}
