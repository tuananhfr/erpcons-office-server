/// Distinct on light and dark backgrounds; members take the first one nobody in the room has.
const COLOURS: [&str; 12] = [
    "#1a73e8", "#d93025", "#188038", "#e37400", "#9334e6", "#007b83", "#c5221f", "#b06000",
    "#3949ab", "#ad1457", "#00897b", "#5f6368",
];

pub(crate) fn pick<'a>(taken: impl Iterator<Item = &'a str>) -> &'static str {
    let taken: Vec<&str> = taken.collect();
    COLOURS
        .iter()
        .copied()
        .find(|c| !taken.contains(c))
        .unwrap_or(COLOURS[taken.len() % COLOURS.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_free_colour_then_wraps() {
        assert_eq!(pick([].into_iter()), COLOURS[0]);
        assert_eq!(pick([COLOURS[0]].into_iter()), COLOURS[1]);
        let all: Vec<&str> = COLOURS.to_vec();
        assert_eq!(pick(all.iter().copied()), COLOURS[0]);
    }
}
