# 
# docker buildx build --tag keesverruijt/mayara-demo:latest .
#
# docker buildx build --platform linux/arm64,linux/amd64 --tag keesverruijt/mayara-demo:latest --push .
#

FROM wdantuma/signalk-radar-demo:latest AS build

WORKDIR /src

RUN apk add --no-cache git curl bash g++ musl-dev openssl-dev openssl pkgconf

RUN curl -sSf https://sh.rustup.rs | bash -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

CMD [ "/bin/sh"]
