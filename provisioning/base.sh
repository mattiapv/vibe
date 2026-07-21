#!/bin/bash
# Base provisioning script that installs mise-en-place and sets up VM disk.
set -euxo pipefail

image_history=/root/vibe-image-history.txt
base_first_run=false
if [[ ! -e "${image_history}" ]]; then
  base_first_run=true
fi

{
  if [[ -s "${image_history}" ]]; then
    echo
  fi
  echo '===== vibe provision ====='
  printf 'provisioned_at_utc: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'image: %s\n' "${VIBE_PROVISION_IMAGE:-unknown}"
  printf 'base: %s\n' "${VIBE_PROVISION_BASE:-unknown}"
  printf 'vibe_git_sha: %s\n' "${VIBE_GIT_SHA:-unknown}"
  printf 'vibe_build_date: %s\n' "${VIBE_BUILD_DATE:-unknown}"
  echo 'scripts:'
  if [[ -n "${VIBE_PROVISION_SCRIPTS:-}" ]]; then
    while IFS= read -r script; do
      printf '  - %s\n' "${script}"
    done <<<"${VIBE_PROVISION_SCRIPTS}"
  fi
} >>"${image_history}"

if [[ "${base_first_run}" != true ]]; then
  exit 0
fi

# Don't wait too long for slow mirrors.
echo 'Acquire::http::Timeout "2";' | tee /etc/apt/apt.conf.d/99timeout
echo 'Acquire::https::Timeout "2";' | tee -a /etc/apt/apt.conf.d/99timeout
echo 'Acquire::Retries "2";' | tee -a /etc/apt/apt.conf.d/99timeout

apt update

##########################################################
# Use debian fast-forward so we get newer package versions

apt install --no-install-recommends --update --yes ca-certificates gnupg debian-keyring wget

mkdir -p /usr/share/debian-fastforward/pgp-keys

wget https://deb.fastforward.debian.net/debian-fastforward/project/pgp/fastforward-debian-13-trixie-signing-key.pub -O /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
wget https://deb.fastforward.debian.net/debian-fastforward/project/pgp/fastforward-debian-13-trixie-signing-key.pub.sig -O /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg.sig

gpg --keyring /usr/share/keyrings/debian-keyring.gpg --keyring /usr/share/keyrings/debian-maintainers.gpg --verify /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg.sig
rm -f /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg.sig

gpg --import /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
rm -f /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
gpg -o /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg --export 2BDDB08FA13971B749E4A221F93CF7F4CEBEC933

sh -c 'cat > /etc/apt/sources.list.d/debian-fastforward.sources << EOF
# /etc/apt/sources.list.d/debian-fastforward.sources

Types: deb
URIs: https://deb.fastforward.debian.net/debian-fastforward
Suites: trixie-fastforward trixie-fastforward-security trixie-fastforward-updates trixie-fastforward-backports
Components: main contrib non-free non-free-firmware
PDiffs: no
Signed-By: /usr/share/debian-fastforward/pgp-keys/deb.fastforward.debian.net.gpg
EOF'

sh -c 'cat > /etc/apt/preferences.d/debian-fastforward.pref << EOF
# /etc/apt/preferences.d/debian-fastforward.pref

Package: *
Pin: release n=trixie-fastforward
Pin-Priority: 990

Package: *
Pin: release n=trixie-fastforward-security
Pin-Priority: 990

Package: *
Pin: release n=trixie-fastforward-updates
Pin-Priority: 990

Package: *
Pin: release n=trixie-fastforward-backports
Pin-Priority: 990
EOF'

apt full-upgrade --update --yes

##########################################################
# End of debian fast forward



apt install --no-install-recommends --yes \
  cloud-guest-utils                       \
  build-essential                         \
  pkg-config                              \
  libssl-dev                              \
  curl                                    \
  git                                     \
  tmux                                    \
  ripgrep


# Expand disk partition
growpart /dev/vda 1

# Expand filesystem
resize2fs /dev/vda1

# Set hostname to "vibe" so it's clear that you're inside the VM.
hostnamectl set-hostname vibe

# Enable SSH when the user created a host Vibe identity.
if [[ -n "${VIBE_SSH_PUBLIC_KEY:-}" ]]; then
  apt install --no-install-recommends --yes openssh-server
  install -d -m 700 /root/.ssh
  printf '%s\n' "${VIBE_SSH_PUBLIC_KEY}" > /root/.ssh/authorized_keys
  chmod 600 /root/.ssh/authorized_keys
  systemctl enable ssh
fi

# Enable true color support in the terminal
echo "export COLORTERM=truecolor" >> .bashrc

# Hide commands beginning with space from the history
echo "export HISTCONTROL=ignorespace" >> .bashrc

# Unlimited bash history
echo "export HISTFILESIZE=" >> .bashrc
echo "export HISTSIZE=" >> .bashrc

# Shutdown the VM when you logout
cat > .bash_logout <<EOF
history -w # Write bash history. Otherwise bash would be killed by poweroff without having written history

# Only shutdown if tmux isn't running
if ! tmux list-sessions &> /dev/null; then
    systemctl poweroff
    sleep 100 # sleep here so that we don't see the login screen flash up before the shutdown.
fi
EOF


# Install Mise
curl https://mise.run | sh
echo 'eval "$(~/.local/bin/mise activate bash)"' >> .bashrc

export PATH="$HOME/.local/bin:$PATH"
eval "$(mise activate bash)"

mkdir -p .config/mise/

cat > .config/mise/config.toml <<MISE
    [settings]
    # Always use the venv created by uv, if available in directory
    python.uv_venv_auto = true

    # Trust everything by default, since we're already in a VM sandbox
    trusted_config_paths = ["/"]

    experimental = true

    [tools]
    uv = "latest"
    node = "latest"

MISE

touch .config/mise/mise.lock
mise install
