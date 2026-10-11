class Logcat < Formula
  desc "Modern, high-performance pidcat replacement in Rust"
  homepage "https://github.com/GBouerat/logcat"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-aarch64-apple-darwin.tar.gz"
      sha256 "6c8b540050d5b0c0472bd4700e2151413bcb8b1b21c301775d51f74d6f96df35"
    else
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-x86_64-apple-darwin.tar.gz"
      sha256 "8f8d44869f15a973370a9b14fb59f31d933276a09e2d4c6991561622cf1f953e"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-aarch64-unknown-linux-musl.tar.gz"
      sha256 "247ad85e52d79e4cac1123af8b13b3f873ca60130c535e86a0033542bb0e23d0"
    else
      url "https://github.com/GBouerat/logcat/releases/download/v#{version}/logcat-x86_64-unknown-linux-musl.tar.gz"
      sha256 "2ef72d354cfcd13d5bdf4e0ec048acac8abd33492bc9cc949d8ddbffc36f6f6a"
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
