class Usagestat < Formula
  desc "Scriptable CLI for local agent usage data"
  homepage "https://github.com/hashimkarim/usagestat"
  version "1.0.3"
  license "MIT"
  depends_on :linux

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/hashimkarim/usagestat/releases/download/v#{version}/usagestat-linux-aarch64.tar.gz"
      sha256 "ca0be657b278267ac704502c6f8a1f404af57c462b80a6d61dcbf5c7fcec7cbb"
    else
      url "https://github.com/hashimkarim/usagestat/releases/download/v#{version}/usagestat-linux-x86_64.tar.gz"
      sha256 "ea74ddbad5a4b1aefc3612947c6e2c4506cd25c452ee245ee4b5fe7a1bb5a4b5"
    end
  end

  def install
    bin.install "usagestat"
    bin.install "usagestatd" if File.exist?("usagestatd")
    (share/"usagestat").install "plugins"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/usagestat --version")
  end
end
