import sys
from lxml import etree

cases = [
    ('amp_plain', 'a&b c'),
    ('amp_two', 'x &y z&q;w'),
    ('amp_known', 'a&amp;b &lt;c&gt;'),
    ('amp_nosemi', 'x&amp y'),
    ('lt_digit', '3<5 x<y'),
    ('lt_tag', 'a<b>c</b>d'),
    ('lt_space', 'a < b > c'),
    ('lt_eof', 'text<unclosed'),
    ('lt_lt', '<<x>>'),
    ('lt_numname', '<1a>v'),
    ('stray_close', 'text</w:t>tail'),
    ('mismatch', '<a><b></a></b>'),
    ('lone_close', 'a</b>c'),
    ('amp_only', '&'),
    ('attr_unq', '<a href=x>t</a>'),
    ('double_amp', 'x && y'),
    ('lt_amp_mix', 'a<b && c>d'),
    ('wrapped_lt', 'V: 3<5 x<y end'),
    ('close_only_amp', '<w:r xmlns:w="urn:w"><w:t>AT&T Inc</w:t></w:r>'),
]
for name, content in cases:
    if name == 'close_only_amp':
        src = content
    else:
        src = '<w:root xmlns:w="urn:w">' + content + '</w:root>'
    p = etree.XMLParser(recover=True)
    try:
        t = etree.fromstring(src.encode(), parser=p)
        out = etree.tostring(t, encoding='unicode') if t is not None else 'NONE'
        sys.stdout.write('== %s => %s\n' % (name, out))
    except Exception as e:
        sys.stdout.write('== %s => EXC %s %s\n' % (name, type(e).__name__, e))
