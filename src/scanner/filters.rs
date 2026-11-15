use rustc_hash::FxHashSet;
use wildmatch::WildMatch;

/// Advanced directory filtering supporting exact folder segment matching and wildcards
#[derive(Clone, Debug)]
pub struct DirFilter {
    exact_names: FxHashSet<String>,
    wildcards: Vec<WildMatch>,
}

impl DirFilter {
    pub fn new(ignore_dirs: &[String]) -> Self {
        let mut exact_names = FxHashSet::default();
        let mut wildcards = Vec::new();

        for s in ignore_dirs {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                continue;
            }
            let s_normalized = trimmed.replace('\\', "/").to_lowercase();
            // Patterns with wildcards or path separators use wildcard matching
            if s_normalized.contains('*')
                || s_normalized.contains('?')
                || s_normalized.contains('/')
            {
                let pattern = if s_normalized.contains('*') || s_normalized.contains('?') {
                    s_normalized
                } else {
                    format!("*{}*", s_normalized)
                };
                wildcards.push(WildMatch::new(&pattern));
            } else {
                // Exact directory names (e.g., ".git", "node_modules")
                exact_names.insert(s_normalized);
            }
        }

        Self {
            exact_names,
            wildcards,
        }
    }

    /// Evaluates if a given path matches the ignore rules
    pub fn is_match(&self, path: &std::path::Path, full_path_lower: &str) -> bool {
        // 1. O(depth) exact segment matching (e.g. any path inside .git or node_modules)
        if !self.exact_names.is_empty() {
            for comp in path.components() {
                let comp_str = comp.as_os_str().to_string_lossy().to_lowercase();
                if self.exact_names.contains(&comp_str) {
                    return true;
                }
            }
        }

        // 2. Fallback to path and wildcard patterns
        !self.wildcards.is_empty() && self.wildcards.iter().any(|m| m.matches(full_path_lower))
    }
}

#[cfg(test)]
mod tests {
    use super::DirFilter;
    use std::path::Path;

    #[test]
    fn exact_dir_name_matches() {
        let filter = DirFilter::new(&[".git".to_string(), "node_modules".to_string()]);
        assert!(filter.is_match(Path::new("foo/.git"), "foo/.git"));
        assert!(filter.is_match(Path::new("a/node_modules/b"), "a/node_modules/b"));
        assert!(!filter.is_match(Path::new("foo/bar"), "foo/bar"));
    }

    #[test]
    fn wildcard_matches() {
        let filter = DirFilter::new(&["*.tmp".to_string()]);
        assert!(filter.is_match(Path::new("a/x.tmp"), "a/x.tmp"));
        assert!(!filter.is_match(Path::new("a/x.txt"), "a/x.txt"));
    }

    proptest::proptest! {
        #[test]
        fn exact_name_always_matches(segments in proptest::collection::vec("[a-z]{1,8}", 1..4)) {
            let name = segments.join("/");
            let filter = DirFilter::new(std::slice::from_ref(&name));
            let path = Path::new(&name);
            proptest::prop_assert!(filter.is_match(path, &name));
        }

        #[test]
        fn wildcard_suffix_matches(name in "[a-z]{1,8}") {
            let filter = DirFilter::new(&["*.tmp".to_string()]);
            let path_str = format!("{}.tmp", name);
            let path = Path::new(&path_str);
            proptest::prop_assert!(filter.is_match(path, &path_str));
        }
    }
}
