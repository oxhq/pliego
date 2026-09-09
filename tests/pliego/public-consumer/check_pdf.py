# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Independent pypdf content check for one completed public consumer stage."""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path

from pypdf import PdfReader

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('report', type=Path)
args = parser.parse_args()
if importlib.metadata.version('pypdf') != '6.16.2':
    raise ValueError('Final PDF proof requires reviewed pypdf 6.16.2')
report = json.loads(args.report.read_bytes())
if report['outcome'] != 'passed':
    raise ValueError('Consumer stage did not pass')
stored = report['stored']
pdf = Path(stored['path'])
if hashlib.sha256(pdf.read_bytes()).hexdigest() != stored['pdf_sha256']:
    raise ValueError('Stored PDF changed')
reader = PdfReader(pdf)
text = ''.join(page.extract_text() for page in reader.pages).strip()
if len(reader.pages) != 1 or text != 'PLIEGO PUBLIC ' + report['stage'] + ' 450.00':
    raise ValueError('PDF page/content differs from expected native scene text')
proof = {'schema': 'pliego.final-public-pdf-check.v1', 'outcome': 'passed',
         'pypdf': importlib.metadata.version('pypdf'), 'pages': len(reader.pages), 'text': text,
         'pdf_sha256': stored['pdf_sha256'], 'report_sha256': hashlib.sha256(args.report.read_bytes()).hexdigest()}
output = args.report.parent / 'independent-pdf.json'
with output.open('x', encoding='utf-8') as stream:
    json.dump(proof, stream, indent=2)
    stream.write('\n')
print(json.dumps(proof))
