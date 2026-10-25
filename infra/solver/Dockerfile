FROM rust:1.97-slim@sha256:3b2879047d42784ca9403ad20c51ed3df361a50f1df96f5777d39b4e33aa65cd AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fixtures ./fixtures
COPY tests/fixtures ./tests/fixtures
RUN cargo build --release --locked -p shadowsolve-solver-engine

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
RUN useradd --create-home --uid 10001 solver
COPY --from=build /app/target/release/shadowsolve-solver-engine /usr/local/bin/solver
RUN mkdir -p /var/lib/shadowsolve && chown -R solver:solver /var/lib/shadowsolve
USER solver
EXPOSE 8080
CMD ["/usr/local/bin/solver"]
