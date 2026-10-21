#!/usr/bin/env python3
"""Bootstrap an offline test/personal mTLS CA. Never installs trust or uploads keys.

For production provisioning, generate runner CSRs on their own computers and
keep the CA private key offline. Application signing/encryption keys are generated
separately with `layeragent remote keygen` on each trusted endpoint.
"""
import argparse
import ipaddress
import json
import pathlib
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', required=True, type=pathlib.Path)
    parser.add_argument('--hostname', required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9.-]{1,253}', args.hostname):
        parser.error('hostname must be an explicit DNS name or IPv4 address')
    directory = args.directory.resolve()
    directory.mkdir(mode=0o700, parents=False, exist_ok=False)

    def openssl(*arguments):
        subprocess.run(['openssl', *map(str, arguments)], check=True,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    openssl('req', '-x509', '-newkey', 'ed25519', '-nodes', '-keyout', directory/'ca.key',
            '-out', directory/'ca.pem', '-days', '365', '-subj', '/CN=LayerAgent personal offline CA',
            '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
    try:
        ipaddress.ip_address(args.hostname)
        san = 'IP:' + args.hostname
    except ValueError:
        san = 'DNS:' + args.hostname
    for name in ['coordinator', 'owner', 'runner']:
        openssl('req', '-new', '-newkey', 'ed25519', '-nodes', '-keyout', directory/f'{name}.key',
                '-out', directory/f'{name}.csr', '-subj', f'/CN=layeragent-{name}')
        extensions = directory/f'{name}.ext'
        extensions.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n' +
                              ('extendedKeyUsage=serverAuth\nsubjectAltName='+san+'\n' if name == 'coordinator'
                               else 'extendedKeyUsage=clientAuth\n'))
        openssl('x509', '-req', '-in', directory/f'{name}.csr', '-CA', directory/'ca.pem',
                '-CAkey', directory/'ca.key', '-CAcreateserial', '-out', directory/f'{name}.pem',
                '-days', '30', '-extfile', extensions)
        (directory/f'{name}.key').chmod(0o600)
    (directory/'ca.key').chmod(0o600)
    print(json.dumps({'directory': str(directory), 'certificate_days': 30,
                      'next': 'Move ca.key offline. Transfer only the appropriate leaf key/cert and public CA to each endpoint.'}))


if __name__ == '__main__':
    main()
