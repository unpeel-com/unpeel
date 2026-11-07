#!/usr/bin/env python3
"""Collect licenses of linked packages from the pinned upstream source/cache."""
import json
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
output = pathlib.Path(sys.argv[2])
packages = json.loads((source / 'deps/libghostty-vt-third-party.json').read_text())['third_party']
# Choose the permissive alternative for dual-licensed dependencies.
license_files = {
    'highway': ['LICENSE-BSD3'],
    'simdutf': ['LICENSE-MIT'],
    'translate_c': ['LICENSE'],
    'uucode': ['LICENSE.md', 'licenses/LICENSE_unicode', 'licenses/LICENSE_Bjoern_Hoehrmann'],
    'wuffs': ['LICENSE-MIT'],
}
sections = ['Bundled libghostty-vt 1.0.0 third-party notices\n'
            'Source: ghostty-org/ghostty vt-1.0.0, 0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec\n',
            'Ghostty (MIT)\n' + (source / 'LICENSE').read_text()]
for package in packages:
    if package['use'] != 'linked':
        continue
    name = package['name']
    root = source / package['path'] if package['kind'] == 'vendored' else source / 'zig-pkg' / package['hash']
    texts = []
    for name_in_package in license_files[name]:
        path = root / name_in_package
        if not path.exists() and name == 'uucode' and name_in_package.startswith('licenses/'):
            # Zig's uucode package omits these files; copied from the same
            # pinned commit (1fb73433...) into this vendor directory.
            path = pathlib.Path(__file__).parent / 'licenses/uucode' / pathlib.Path(name_in_package).name
        text = path.read_text()
        if not text.strip():
            raise ValueError(f'Empty license: {root / name_in_package}')
        texts.append(f'{name_in_package}\n{text}')
    sections.append(f"{name} {package['version']} ({package['license']})\n"
                    + '\n'.join(texts))
output.write_text(('\n' + '=' * 80 + '\n\n').join(sections))
