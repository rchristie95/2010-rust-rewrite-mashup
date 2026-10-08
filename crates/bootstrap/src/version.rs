pub fn build_line() -> String {
    let profile = env!("IW4L_BUILD_PROFILE");
    let profile = if profile.is_empty() {
        env!("IW4L_BUILD_PROFILE_KIND")
    } else {
        profile
    };
    line_from(
        env!("IW4L_BUILD_GIT_DESCRIBE"),
        env!("IW4L_BUILD_GIT_SHA"),
        env!("IW4L_BUILD_GIT_DIRTY"),
        profile,
        env!("IW4L_BUILD_RUSTC"),
    )
}

fn line_from(describe: &str, sha: &str, dirty: &str, profile: &str, rustc: &str) -> String {
    let version = if !describe.is_empty() {
        describe
    } else if !sha.is_empty() {
        &sha[..sha.len().min(7)]
    } else {
        "unknown"
    };
    let mut parts = vec!["build", version];
    if !profile.is_empty() {
        parts.push(profile);
    }
    if dirty == "true" && !version.ends_with("-dirty") {
        parts.push("dirty");
    }
    if !rustc.is_empty() {
        parts.push(rustc);
    }
    parts.join(" ")
}
