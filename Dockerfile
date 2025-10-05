FROM ubuntu:latest

RUN apt update -y && apt install -y rustc libssl-dev pkg-config strace less

