class Logcat < Formula
  desc "Modern, high-performance pidcat replacement in Rust"
  homepage "https://github.com/GBouerat/logcat"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-aarch64-apple-darwin.tar.gz"
      # sha256 "REPLACE_WITH_MACOS_ARM64_SHA256"
    else
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-x86_64-apple-darwin.tar.gz"
      # sha256 "REPLACE_WITH_MACOS_X86_64_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-aarch64-unknown-linux-musl.tar.gz"
      # sha256 "REPLACE_WITH_LINUX_ARM64_SHA256"
    else
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-x86_64-unknown-linux-musl.tar.gz"
      # sha256 "REPLACE_WITH_LINUX_X86_64_SHA256"
    end
  end

  def install
    bin.install "logcat"

    # Install shell completions directly from the compiled binary
    generate_completions_from_executable(bin/"logcat", "--generate-completions")
  end

  test do
    assert_match "logcat #{version}", shell_output("#{bin}/logcat --version")
  end
end
