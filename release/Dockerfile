# Compila Linux e Windows no Linux, sem precisar do Windows (`release/release.sh`).
# Ubuntu 24.04 de propósito: o binário Linux sai pedindo a glibc dele (2.39), e não a do PC
# de quem compila (2.43 no Ubuntu 26.04), que o Ubuntu 24.04 e o Debian 13 não abrem.
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl wget gnupg lsb-release software-properties-common build-essential \
        pkg-config libasound2-dev libglib2.0-dev libx11-dev \
        libxext-dev libxfixes-dev libxdamage-dev libxrandr-dev libxcomposite-dev libxtst-dev \
        libxkbcommon-dev libwayland-dev libudev-dev libdrm-dev libgbm-dev libva-dev \
        libpipewire-0.3-dev libdbus-1-dev \
    # O libc++ que vem com o libwebrtc pede clang 21+. O lld-21 traz o `lld-link` do Windows.
    && wget -q https://apt.llvm.org/llvm.sh && bash llvm.sh 21 && rm llvm.sh \
    && apt-get install -y --no-install-recommends llvm-21 \
    && rm -rf /var/lib/apt/lists/*

# clang-cl, lld-link, llvm-lib e llvm-rc (ícone do .exe) sem sufixo de versão.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/lib/llvm-21/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
        --default-toolchain 1.96.0 -c clippy,rustfmt -t x86_64-pc-windows-msvc \
    && cargo install --locked cargo-xwin --version 0.23.1 \
    && rm -rf /opt/cargo/registry /opt/cargo/git \
    # O container roda com o uid de quem chama: o cargo grava trava e cache em CARGO_HOME.
    && chmod -R a+rX /opt/rustup && chmod a+rwx /opt/cargo
