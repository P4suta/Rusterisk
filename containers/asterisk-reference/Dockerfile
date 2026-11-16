FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        build-essential=12.12 pkg-config=1.8.1-4 autoconf-archive=20220903-3 \
        bzip2=1.0.8-6 patch=2.8-2 ca-certificates=20250419+deb13u1 \
        curl=8.14.1-2+deb13u7 wget=1.25.0-2 libedit-dev=3.1-20250104-1 \
        libjansson-dev=2.14-2+b3 libsqlite3-dev=3.46.1-7+deb13u2 uuid-dev=2.41.5-0+deb13u1 \
        libxml2-dev=2.12.7+dfsg+really2.9.14-2.1+deb13u3 libssl-dev=3.5.7-1~deb13u3 \
        libsrtp2-dev=2.7.0-3 libcurl4-openssl-dev=8.14.1-2+deb13u7 \
        libspeex-dev=1.2.1-3 libspeexdsp-dev=1.2.1-3 libopus-dev=1.5.2-2 \
        libspandsp-dev=0.0.6+dfsg-2.2 libgsm1-dev=1.0.22-1+b2 \
        libsndfile1-dev=1.2.2-2+deb13u1 libncurses-dev=6.5+20250216-2 python3=3.13.5-1 \
        cmake=3.31.6-2 libgtest-dev=1.16.0-1 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src
RUN curl --fail --location --max-time 120 --output asterisk.tar.gz \
        https://downloads.asterisk.org/pub/telephony/asterisk/releases/asterisk-22.9.0.tar.gz \
    && printf '%s  %s\n' 5fd0e623d7415faa5999c1d5797721a3ab8992cbe33d49faac7d948635df94cc asterisk.tar.gz | sha256sum --check --strict \
    && tar --extract --gzip --file asterisk.tar.gz \
    && rm asterisk.tar.gz

RUN curl --fail --location --max-time 120 --output sipp.tar.gz \
        https://github.com/SIPp/sipp/releases/download/v3.8.0/sipp-3.8.0.tar.gz \
    && printf '%s  %s\n' a3e3993bf9369caaae1f5366507d429acb9669dcf678cfacfd01c907102f74c3 sipp.tar.gz | sha256sum --check --strict \
    && tar --extract --gzip --file sipp.tar.gz \
    && cmake -S /usr/src/sipp-3.8.0 -B /usr/src/sipp-build \
        -DCMAKE_BUILD_TYPE=Release -DUSE_SCTP=OFF -DUSE_PCAP=OFF -DUSE_GSL=OFF \
        -DUSE_LUA=OFF -DUSE_SYSTEM_PUGIXML=OFF -DUSE_SYSTEM_GTEST=ON \
    && timeout 300 cmake --build /usr/src/sipp-build --parallel 2 --target sipp sipp_unittest \
    && timeout 120 ctest --test-dir /usr/src/sipp-build --output-on-failure --timeout 30 \
    && install -m 755 /usr/src/sipp-build/sipp /usr/local/bin/sipp \
    && if /usr/local/bin/sipp -v > /tmp/sipp-version.txt; then false; else test "$?" -eq 99; fi \
    && grep --fixed-strings --line-regexp ' SIPp v3.8.0-TLS.' /tmp/sipp-version.txt \
    && rm -rf /usr/src/sipp-3.8.0 /usr/src/sipp-build /usr/src/sipp.tar.gz /tmp/sipp-version.txt

WORKDIR /usr/src/asterisk-22.9.0
RUN timeout 300 ./configure --prefix=/usr --sbindir=/usr/sbin --sysconfdir=/etc --localstatedir=/var \
        --with-pjproject-bundled --with-jansson --without-dahdi --without-pri --without-tonezone \
    && make menuselect.makeopts \
    && menuselect/menuselect --disable BUILD_NATIVE \
        --disable-category MENUSELECT_CORE_SOUNDS --disable-category MENUSELECT_EXTRA_SOUNDS \
        --disable-category MENUSELECT_MOH menuselect.makeopts \
    && timeout 900 make --jobs=2 \
    && timeout 120 make install \
    && test "$(/usr/sbin/asterisk -V)" = 'Asterisk 22.9.0'

WORKDIR /
RUN rm -rf /usr/src/asterisk-22.9.0 \
    && install -d -o 65532 -g 65532 /var/run/asterisk /var/spool/asterisk /var/log/asterisk /var/cache/asterisk \
    && chown -R 65532:65532 /var/lib/asterisk

USER 65532:65532
ENTRYPOINT ["/usr/sbin/asterisk", "-f", "-g", "-vvv", "-C", "/etc/asterisk/asterisk.conf"]
