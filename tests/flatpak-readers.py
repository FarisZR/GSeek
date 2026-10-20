"""Make small real files for the installed sandbox's document/media readers."""
from pathlib import Path
import sys
import zipfile
root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
stream = b'BT /F1 12 Tf 50 150 Td (GSeek PDF reader) Tj ET'
objects = [b'<< /Type /Catalog /Pages 2 0 R >>', b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
 b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
 b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>', b'<< /Length '+str(len(stream)).encode()+b' >>\nstream\n'+stream+b'\nendstream']
pdf = bytearray(b'%PDF-1.4\n')
offsets = [0]
for number, obj in enumerate(objects, 1):
 offsets.append(len(pdf)); pdf.extend(f'{number} 0 obj\n'.encode()+obj+b'\nendobj\n')
xref = len(pdf)
pdf.extend(f'xref\n0 {len(offsets)}\n0000000000 65535 f \n'.encode())
for offset in offsets[1:]: pdf.extend(f'{offset:010d} 00000 n \n'.encode())
pdf.extend(f'trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
(root / 'sample.pdf').write_bytes(pdf)
with zipfile.ZipFile(root / 'sample.docx', 'w') as archive:
 archive.writestr('word/document.xml', '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>GSeek Office reader</w:t></w:r></w:p></w:body></w:document>')
