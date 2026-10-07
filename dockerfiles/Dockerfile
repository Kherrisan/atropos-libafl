# Atropos LibAFL Nyx fuzzing image.
#
# Build from the directory that contains atropos-libafl and wordpress
# (Docker cannot COPY paths outside the build context):
#
#   docker build -f atropos-libafl/dockerfiles/Dockerfile -t atropos-libafl .
#
# The image builds the fuzzer, the PHP guest runtime, and the Spring guest
# runtime. The first container
# start packages that runtime with the WordPress tree and creates the Ubuntu
# disk. MariaDB installation, database initialization, and wp_install run
# inside that guest during cloud-init. The disk and Nyx pre-snapshot need
# /dev/kvm and are stored under /var/lib/atropos/nyx.
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
# are forwarded to scripts/atropos.sh run.

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C \
    ATROPOS_NYX_DATA_DIR=/var/lib/atropos/nyx \
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
# holding atropos-libafl and wordpress side by side.
ARG REPO_SRC=atropos-libafl
ARG WORDPRESS_SRC=wordpress

COPY ${REPO_SRC} /opt/atropos-libafl
COPY ${WORDPRESS_SRC} /opt/wordpress

WORKDIR /opt/atropos-libafl

# The SpringFuzz submodule is not always present in the build context.
ARG SPRINGFUZZ_URL=https://github.com/H4de5-7/SpringFuzz.git
ARG SPRINGFUZZ_REV=8387aeaf736d08959e2d5e598e9b6e0ee6d18455
RUN if [ ! -f SpringFuzz/pom.xml ]; then \
        git clone "$SPRINGFUZZ_URL" SpringFuzz \
        && git -C SpringFuzz checkout "$SPRINGFUZZ_REV"; \
    fi

RUN chmod 0755 dockerfiles/entrypoint.sh scripts/*.sh \
    && mkdir -p /var/lib/atropos/nyx \
    && bash scripts/atropos.sh build-fuzzer \
    && bash scripts/atropos.sh build-php \
        --php-output /var/lib/atropos/nyx/guest \
        --fuzzer-output /var/lib/atropos/nyx \
    && bash scripts/atropos.sh build-spring \
        --springfuzz-src /opt/atropos-libafl/SpringFuzz \
        --fuzzer-output /var/lib/atropos/nyx

ENTRYPOINT ["/opt/atropos-libafl/dockerfiles/entrypoint.sh"]
