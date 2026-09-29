FROM fedora:44

RUN dnf install -y \
    cargo \
    copr-cli \
    gcc \
    git \
    openssl-devel \
    pkgconf-pkg-config \
    python3 \
    rpm-build \
    rpmdevtools \
    rust \
    systemd-rpm-macros \
    && dnf clean all

WORKDIR /work

CMD ["/bin/bash"]
