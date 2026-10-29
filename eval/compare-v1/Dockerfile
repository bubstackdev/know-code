# compare-v1's container (README.md, "The container"): one image per language, on that language's identity-v1
# toolchain image, holding KnowCode, codebase-memory-mcp and Serena. scripts/eval/compare/build.sh passes every
# argument from tools.toml; nothing here is fetched without a digest to check it against. The image is local only.
#
# The build context holds `knowcode` (the Linux build under test) and nothing else.
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ARG LANGUAGE
ARG ARCH
ARG UV_URL
ARG UV_SHA256
ARG CBM_URL
ARG CBM_SHA256
ARG SERENA_URL
ARG SERENA_SHA256
ARG SERENA_PYTHON
ENV DEBIAN_FRONTEND=noninteractive DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 KNOWCODE_UPDATE_CHECK=0 \
    UV_PYTHON_INSTALL_DIR=/opt/python UV_NO_CACHE=1 UV_LINK_MODE=copy

# What every tool needs, and the .NET SDK for the C# image (Ubuntu's archive, as identity-v1's labeller installs it;
# Serena's Roslyn language server runs on it).
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends ca-certificates curl git jq \
 && if [ "$LANGUAGE" = csharp ]; then apt-get install -y -qq --no-install-recommends dotnet-sdk-10.0; fi \
 && rm -rf /var/lib/apt/lists/*

# uv, which installs Serena from its lock file: the release asset, checked against its digest.
RUN curl -fsSL -o /tmp/uv.tar.gz "$UV_URL" \
 && echo "$UV_SHA256  /tmp/uv.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/uv.tar.gz -C /usr/local/bin --strip-components=1 && rm /tmp/uv.tar.gz

# codebase-memory-mcp: the release asset, checked against its digest.
RUN curl -fsSL -o /tmp/cbm.tar.gz "$CBM_URL" \
 && echo "$CBM_SHA256  /tmp/cbm.tar.gz" | sha256sum -c - \
 && mkdir -p /opt/cbm && tar -xzf /tmp/cbm.tar.gz -C /opt/cbm && rm /tmp/cbm.tar.gz \
 && ln -s /opt/cbm/codebase-memory-mcp /usr/local/bin/codebase-memory-mcp

# Serena: the source distribution, checked against its digest, installed with its own lock file.
RUN curl -fsSL -o /tmp/serena.tar.gz "$SERENA_URL" \
 && echo "$SERENA_SHA256  /tmp/serena.tar.gz" | sha256sum -c - \
 && mkdir -p /opt/serena && tar -xzf /tmp/serena.tar.gz -C /opt/serena --strip-components=1 && rm /tmp/serena.tar.gz \
 && cd /opt/serena && uv python install "$SERENA_PYTHON" \
 && uv sync --frozen --no-dev --python "$SERENA_PYTHON" \
 && ln -s /opt/serena/.venv/bin/serena /usr/local/bin/serena \
 && ln -s "$(uv python find "$SERENA_PYTHON")" /usr/local/bin/compare-python

# Serena's language servers, fetched now as its documentation advises for restricted networks. Swift's
# (sourcekit-lsp) ships with the toolchain image.
RUN case "$LANGUAGE" in \
      csharp) serena download-ls-dependencies csharp ;; \
      typescript) serena download-ls-dependencies typescript ;; \
    esac

COPY knowcode /usr/local/bin/knowcode
RUN knowcode --version && codebase-memory-mcp --version && serena --help >/dev/null

WORKDIR /work
