set dotenv-load := true

build *args:
  cargo build -- {{ args }}

build-container: build
  podman build -f Containerfile -t ftp-paperless-bridge:local target/debug

run *args:
  cargo run -- {{ args }}

build-release:
  cargo build --release

build-musl-x86_64:
  cargo build --release --target x86_64-unknown-linux-musl

build-musl-aarch64:
  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-unknown-linux-musl-gcc cargo build --release --target aarch64-unknown-linux-musl

# Build APK package using melange
build-apk arch="aarch64":
  mkdir -p "${HOME}/.cargo/registry" "${HOME}/.cargo/git"
  melange build melange.yaml --arch {{arch}} --cache-dir "${HOME}/.cargo"

# Clean melange build artifacts
clean-apk:
  rm -rf packages/

# Deploy APK to remote Alpine host (using melange)
deploy-alpine host="alpine" arch="aarch64":
  #!/usr/bin/env bash
  set -e

  # Check if packages directory exists
  if [ ! -d "packages/{{arch}}" ]; then
    echo "No packages found for {{arch}}, building..."
    just build-apk {{arch}}
  fi

  # Find the APK file
  APK_FILE=$(find packages/{{arch}}/ -name "ftp-paperless-bridge-*.apk" 2>/dev/null | head -n1)

  # If no APK found, build it
  if [ -z "$APK_FILE" ]; then
    echo "APK not found, building it first..."
    just build-apk {{arch}}
    APK_FILE=$(find packages/{{arch}}/ -name "ftp-paperless-bridge-*.apk" 2>/dev/null | head -n1)
    if [ -z "$APK_FILE" ]; then
      echo "Failed to build APK"
      exit 1
    fi
  fi

  echo "📦 Deploying $APK_FILE to {{host}}..."

  # Copy APK to remote host
  scp "$APK_FILE" root@{{host}}:/tmp/

  # Install APK and configure service
  ssh root@{{host}} <<'EOF'
    set -e
    echo "Installing APK package..."
    apk add --allow-untrusted /tmp/ftp-paperless-bridge-*.apk

    echo "Configuring service..."
    if [ ! -f /etc/conf.d/ftp-paperless-bridge.configured ]; then
      echo "Please edit /etc/conf.d/ftp-paperless-bridge with your settings"
      echo "Then run: rc-service ftp-paperless-bridge start"
      echo "To enable at boot: rc-update add ftp-paperless-bridge default"
      touch /etc/conf.d/ftp-paperless-bridge.configured
    else
      echo "Configuration already exists, restarting service..."
      rc-service ftp-paperless-bridge restart || true
    fi

    # Clean up
    rm /tmp/ftp-paperless-bridge-*.apk

    echo "✅ Deployment complete!"
  EOF
