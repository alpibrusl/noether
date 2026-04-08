//! `noether build --target react-native` — generate a React Native / Expo project
//! that wraps the browser build (WASM + HTML) inside a full-screen WebView.
//!
//! ## What is produced
//!
//! ```
//! <output_dir>/
//!   assets/
//!     index.html        ← browser build entry point
//!     noether_bg.wasm   ← compiled stage graph
//!     noether.js        ← wasm-bindgen JS glue
//!   App.tsx             ← React Native root component
//!   app.json            ← Expo configuration
//!   package.json        ← npm/yarn dependencies
//!   tsconfig.json       ← TypeScript config
//!   README.md           ← usage instructions
//! ```
//!
//! ## How it works
//!
//! 1. Delegates to `cmd_build_browser` to produce WASM + HTML in a temp directory.
//! 2. Copies those artifacts into `<output_dir>/assets/`.
//! 3. Generates the React Native / Expo scaffolding files.
//!
//! Requires Node.js + Expo CLI for running (`npx expo start`).

use super::build::BuildOptions;
use super::build_browser::cmd_build_browser;
use crate::output::{acli_error, acli_ok};
use std::path::Path;

pub fn cmd_build_mobile(store: &dyn noether_store::StageStore, opts: BuildOptions<'_>) {
    let output_path = Path::new(opts.output_path);
    let assets_dir = output_path.join("assets");

    // ── 1. Build the browser bundle into a temp assets directory ─────────────
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let browser_tmp = std::env::temp_dir().join(format!("noether-mobile-browser-{ts}"));

    eprintln!("Building browser bundle for React Native target…");

    // Temporarily redirect browser output to temp dir
    let browser_opts = BuildOptions {
        graph_path: opts.graph_path,
        output_path: browser_tmp.to_str().unwrap_or("/tmp/noether-mobile-browser"),
        app_name: opts.app_name,
        description: opts.description,
        target: "browser",
        serve_addr: None,
    };
    cmd_build_browser(store, browser_opts);

    // ── 2. Create output directory structure ──────────────────────────────────
    if let Err(e) = std::fs::create_dir_all(&assets_dir) {
        eprintln!("{}", acli_error(&format!("Failed to create assets dir: {e}")));
        std::process::exit(1);
    }

    let write_file = |path: &Path, contents: &str| {
        if let Err(e) = std::fs::write(path, contents) {
            eprintln!("{}", acli_error(&format!("Failed to write {}: {e}", path.display())));
            std::process::exit(1);
        }
    };

    // ── 3. Copy browser artifacts into assets/ ────────────────────────────────
    for filename in &["index.html", "noether_bg.wasm", "noether.js"] {
        let src = browser_tmp.join(filename);
        let dst = assets_dir.join(filename);
        if src.exists() {
            if let Err(e) = std::fs::copy(&src, &dst) {
                eprintln!("{}", acli_error(&format!("Failed to copy {filename}: {e}")));
                std::process::exit(1);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&browser_tmp);

    // ── 4. Resolve app metadata ───────────────────────────────────────────────
    let app_name = opts
        .app_name
        .map(String::from)
        .or_else(|| {
            output_path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "NoetherApp".to_string());

    // Expo slug: lowercase, hyphens only
    let slug: String = app_name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();

    let app_version = env!("CARGO_PKG_VERSION");

    // ── 5. Generate project files ─────────────────────────────────────────────
    write_file(&output_path.join("package.json"), &generate_package_json(&slug, app_version));
    write_file(&output_path.join("app.json"), &generate_app_json(&app_name, &slug));
    write_file(&output_path.join("tsconfig.json"), TSCONFIG);
    write_file(&output_path.join("App.tsx"), &generate_app_tsx(&app_name));
    write_file(&output_path.join("README.md"), &generate_readme(&app_name));

    println!(
        "{}",
        acli_ok(serde_json::json!({
            "output_dir": output_path.display().to_string(),
            "app_name": app_name,
            "version": app_version,
            "files": [
                "assets/index.html",
                "assets/noether_bg.wasm",
                "assets/noether.js",
                "App.tsx",
                "app.json",
                "package.json",
                "tsconfig.json",
                "README.md"
            ],
            "next_steps": [
                "cd ".to_owned() + output_path.to_str().unwrap_or("."),
                "yarn install (or npm install)",
                "npx expo start"
            ]
        }))
    );
}

// ── File generators ───────────────────────────────────────────────────────────

fn generate_package_json(slug: &str, version: &str) -> String {
    format!(
        r#"{{
  "name": "{slug}",
  "version": "{version}",
  "main": "node_modules/expo/AppEntry.js",
  "scripts": {{
    "start": "expo start",
    "android": "expo start --android",
    "ios": "expo start --ios",
    "web": "expo start --web"
  }},
  "dependencies": {{
    "expo": "~51.0.0",
    "expo-status-bar": "~1.12.1",
    "react": "18.2.0",
    "react-native": "0.74.5",
    "react-native-webview": "13.8.6"
  }},
  "devDependencies": {{
    "@babel/core": "^7.20.0",
    "@types/react": "~18.2.45",
    "typescript": "^5.1.3"
  }},
  "private": true
}}
"#
    )
}

fn generate_app_json(name: &str, slug: &str) -> String {
    // Note: hex colors and .png extensions can't appear directly in format!() — build manually.
    let mut s = String::from("{\n  \"expo\": {\n");
    s.push_str(&format!("    \"name\": \"{name}\",\n"));
    s.push_str(&format!("    \"slug\": \"{slug}\",\n"));
    s.push_str("    \"version\": \"1.0.0\",\n");
    s.push_str("    \"orientation\": \"portrait\",\n");
    s.push_str("    \"userInterfaceStyle\": \"dark\",\n");
    s.push_str("    \"assetBundlePatterns\": [\"assets/**\"],\n");
    s.push_str("    \"ios\": { \"supportsTablet\": true },\n");
    s.push_str("    \"android\": { \"adaptiveIcon\": { \"backgroundColor\": \"#0a0d0f\" } },\n");
    s.push_str("    \"web\": { \"favicon\": \"./assets/favicon.png\" }\n");
    s.push_str("  }\n}\n");
    s
}

fn generate_app_tsx(app_name: &str) -> String {
    format!(
        r#"import React, {{ useRef }} from 'react';
import {{ StatusBar }} from 'expo-status-bar';
import {{ StyleSheet, View, Platform }} from 'react-native';
import WebView from 'react-native-webview';
import {{ Asset }} from 'expo-asset';

// Resolve the bundled HTML asset path at runtime.
// On iOS/Android, assets are bundled by Expo and accessible via file:// URIs.
// On Web, the browser build is served directly.
const HTML_ASSET = require('./assets/index.html');

export default function App() {{
  const webViewRef = useRef<WebView>(null);

  // Inject a bridge so the NoetherRuntime can call native navigation APIs.
  const injectedJS = `
    window._noetherNative = {{
      navigate: (path) => window.ReactNativeWebView.postMessage(
        JSON.stringify({{ type: 'navigate', path }})
      ),
    }};
    true;
  `;

  const onMessage = (event: any) => {{
    try {{
      const msg = JSON.parse(event.nativeEvent.data);
      if (msg.type === 'navigate') {{
        // Handle deep-link / back navigation here if needed
      }}
    }} catch (_) {{}}
  }};

  if (Platform.OS === 'web') {{
    // On web, render the Noether app directly in an iframe.
    return (
      <View style={{styles.container}}>
        <iframe
          src="./assets/index.html"
          style={{{{ width: '100%', height: '100%', border: 'none' }}}}
          title="{app_name}"
        />
        <StatusBar style="light" />
      </View>
    );
  }}

  return (
    <View style={{styles.container}}>
      <WebView
        ref={{webViewRef}}
        source={{{{ uri: Asset.fromModule(HTML_ASSET).localUri || '' }}}}
        originWhitelist={{['*']}}
        allowFileAccess
        allowUniversalAccessFromFileURLs
        injectedJavaScript={{injectedJS}}
        onMessage={{onMessage}}
        style={{styles.webview}}
      />
      <StatusBar style="light" />
    </View>
  );
}}

const styles = StyleSheet.create({{
  container: {{
    flex: 1,
    backgroundColor: '#0a0d0f',
  }},
  webview: {{
    flex: 1,
    backgroundColor: '#0a0d0f',
  }},
}});
"#,
        app_name = app_name
    )
}

fn generate_readme(app_name: &str) -> String {
    format!(
        r#"# {app_name} — React Native App

Generated by `noether build --target react-native`.

## Prerequisites

- Node.js 18+
- Expo CLI: `npm install -g expo-cli`
- For iOS: Xcode + iOS Simulator
- For Android: Android Studio + emulator

## Running

```bash
yarn install    # or: npm install
npx expo start  # shows QR code for Expo Go
```

Press `i` for iOS simulator, `a` for Android emulator, `w` for web browser.

## How it works

The app renders your Noether composition in a full-screen WebView.
The Noether stage graph is compiled to WebAssembly and runs directly in the
WebView's JavaScript engine — no network call needed for local stages.

`RemoteStage` nodes in your graph will call your backend API via `fetch()`,
same as the desktop browser build.

## Updating the app

After modifying your Noether graph, rebuild and copy assets:

```bash
noether build <graph.json> --target react-native --output .
# then restart Expo: npx expo start --clear
```
"#
    )
}

const TSCONFIG: &str = r#"{
  "extends": "expo/tsconfig.base",
  "compilerOptions": {
    "strict": true
  }
}
"#;
