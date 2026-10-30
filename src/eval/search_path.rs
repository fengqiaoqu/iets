//! The entries of `-I`, `NIX_PATH` and `nix-path`, as Nix reads them
//! (eval-settings.cc). A value is a path unless it is a URL, and a URL is
//! not split at its colon.

/// eval-settings.cc:isPseudoUrl, plus a `flake:` reference: a value Nix
/// fetches instead of reading. iets does not fetch search path entries.
pub fn is_url(value: &[u8]) -> bool {
    if value.starts_with(b"channel:") || value.starts_with(b"flake:") {
        return true;
    }
    let Some(at) = value.windows(3).position(|w| w == b"://") else {
        return false;
    };
    matches!(
        &value[..at],
        b"http" | b"https" | b"file" | b"channel" | b"git" | b"s3" | b"ssh"
    )
}

/// eval-settings.cc:parseNixPath: `NIX_PATH` split at `:`, except that a
/// value which is a URL keeps its next segment. Empty entries are dropped.
pub fn split(s: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let rest = &s[start..];
        let mut end = rest.find(':').unwrap_or(rest.len());
        if end < rest.len() {
            // Nix tests the text after the last `=` of the entry up to the
            // end of the whole string
            let value = rest[..end].rfind('=').map_or(0, |i| i + 1);
            if is_url(&rest.as_bytes()[value..]) {
                end = rest[end + 1..]
                    .find(':')
                    .map_or(rest.len(), |i| end + 1 + i);
            }
        }
        if end > 0 {
            entries.push(&rest[..end]);
        }
        start += end + 1;
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_what_nix_fetches() {
        for v in [
            "flake:nixpkgs",
            "channel:nixos-unstable",
            "https://x/y.tar.xz",
            "file:///x",
            "git://h/r",
            "s3://b/k",
            "ssh://h/r",
        ] {
            assert!(is_url(v.as_bytes()), "{v}");
        }
        for v in [
            "/nix/var/nix/profiles/per-user/root/channels",
            "nixpkgs",
            "ftp://x/y",
            "http",
            "./flake:x",
        ] {
            assert!(!is_url(v.as_bytes()), "{v}");
        }
    }

    #[test]
    fn a_url_keeps_its_colon() {
        assert_eq!(split("a=/x:b=/y"), ["a=/x", "b=/y"]);
        assert_eq!(
            split("nixpkgs=flake:nixpkgs:b=/y"),
            ["nixpkgs=flake:nixpkgs", "b=/y"]
        );
        assert_eq!(split("flake:nixpkgs"), ["flake:nixpkgs"]);
        assert_eq!(
            split("a=/x:nixpkgs=channel:nixos-unstable"),
            ["a=/x", "nixpkgs=channel:nixos-unstable"]
        );
        assert_eq!(
            split("nixpkgs=https://x/y.tar.xz:b=/y"),
            ["nixpkgs=https://x/y.tar.xz", "b=/y"]
        );
        assert_eq!(split("https://x/y.tar.xz"), ["https://x/y.tar.xz"]);
        assert_eq!(split(":a=/x::b=/y:"), ["a=/x", "b=/y"]);
        assert!(split("").is_empty());
    }
}
