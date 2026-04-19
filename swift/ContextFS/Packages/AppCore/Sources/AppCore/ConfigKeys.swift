import Foundation

/// Canonical config.toml key names used by the app helper and Swift UI layer.
public enum ConfigKey {
    public static let githubToken = "github_token"
    public static let backend = "backend"
    public static let cacheMaxBytes = "cache_max_bytes"
}

/// Canonical UserDefaults key names.
public enum UserDefaultsKey {
    public static let onboardingComplete = "onboarding_complete"
}
