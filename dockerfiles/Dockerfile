# Atropos LibAFL Nyx fuzzing image.
#
# Build from the directory that contains atropos-libafl, atropos-legacy, and
# wordpress (Docker cannot COPY paths outside the build context):
#
#   docker build -f atropos-libafl/dockerfiles/Dockerfile -t atropos-libafl .
#
# The image builds the fuzzer and the PHP guest runtime. Creating the Ubuntu
# guest disk and the Nyx pre-snapshot needs /dev/kvm, so that step runs in the
# entrypoint on first start and is stored under /var/lib/atropos/nyx.
#
# Do not reload the host KVM module from a container. The host must already
# have enable_vmware_backdoor=Y. Reloading KVM stops every other instance.
#
# One container is one QEMU worker. Share the Nyx data volume, and give each
# instance its own CPU id and CPU set:
#
#   docker run --device /dev/kvm --group-add kvm \
#     -e ATROPOS_NYX_CPU=0 -e ATROPOS_NYX_CPUSET=2-5 \
#     -v atropos-nyx:/var/lib/atropos/nyx \
#     atropos-libafl
#
# The first container must be able to write the volume. Later containers can
# mount the same snapshot. Pass fuzzer arguments after the image name; they
# are forwarded to scripts/run-fuzzer.sh.

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C \
    ATROPOS_NYX_DATA_DIR=/var/lib/atropos/nyx \
    ATROPOS_LEGACY_ROOT=/opt/atropos-legacy \
    ATROPOS_WORDPRESS_ROOT=/opt/wordpress \
    PATH=/root/.cargo/bin:/root/.nix-profile/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update && apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        curl \
        git \
        openssl \
        patch \
        pkg-config \
        python3 \
        ripgrep \
        sudo \
        util-linux \
        uuid-runtime \
        xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system kvm || true

# Single-user Nix. The existing scripts re-enter nix-shell for PHP, QEMU, and
# MariaDB, using the package set in nyx-shell.nix (Nixpkgs 22.11).
RUN mkdir -p /etc/nix \
    && printf 'sandbox = false\nfilter-syscalls = false\nexperimental-features = nix-command flakes\n' > /etc/nix/nix.conf \
    && curl -L https://nixos.org/nix/install -o /tmp/nix-install \
    && bash /tmp/nix-install --no-daemon \
    && rm -f /tmp/nix-install

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | bash -s -- -y --default-toolchain stable

# Names inside the build context. The default layout is a parent directory
# holding atropos-libafl, atropos-legacy, and wordpress side by side.
ARG REPO_SRC=atropos-libafl
ARG LEGACY_SRC=atropos-legacy
ARG WORDPRESS_SRC=wordpress

COPY ${REPO_SRC} /opt/atropos-libafl
COPY ${LEGACY_SRC} /opt/atropos-legacy
COPY ${WORDPRESS_SRC} /opt/wordpress

WORKDIR /opt/atropos-libafl

RUN chmod 0755 dockerfiles/entrypoint.sh scripts/*.sh \
    && mkdir -p /var/lib/atropos/nyx \
    && bash scripts/build-nyx-fuzzer.sh \
    && bash scripts/build-nyx-php.sh \
    && bash scripts/setup-wordpress.sh

ENTRYPOINT ["/opt/atropos-libafl/dockerfiles/entrypoint.sh"]
