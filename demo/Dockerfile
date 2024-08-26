# 
# docker buildx build --tag keesverruijt/mayara-demo:latest .
#
# docker buildx build --platform linux/arm64,linux/amd64 --tag keesverruijt/mayara-demo:latest --push .
#

FROM wdantuma/signalk-radar-demo:latest AS build

RUN apk add --no-cache git curl bash g++

WORKDIR /src

RUN curl -sSf https://sh.rustup.rs | bash -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

RUN git clone https://github.com/keesverruijt/mayara.git && \
    cd mayara && cargo build --release

FROM wdantuma/signalk-radar-demo:latest

RUN apk add --no-cache tcpreplay curl unzip

WORKDIR /app

COPY start.sh .
COPY samples/halo_and_0183.pcap .

RUN chmod +x start.sh

COPY --from=build /src/mayara/target/release/mayara /app/mayara

RUN apk add strace tcpdump

CMD [ "/app/start.sh"]
