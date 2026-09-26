import sys
from lxml import etree

W = 'xmlns:w="urn:w"'
cases = [
    ('stray_in_wt', '<w:root %s><w:r><w:t>a</w:t></w:r><w:r><w:t>a</b>c</w:t>TAIL</w:r></w:root>' % W),
    ('amp_only_in_wt', '<w:root %s><w:r><w:t>&</w:t></w:r><w:r><w:t>OK</w:t></w:r></w:root>' % W),
    ('amp_then_structure', '<w:root %s><w:r><w:t>x&y</w:t></w:r><w:r><w:t>z</w:t></w:r></w:root>' % W),
    ('unclosed_at_parent_end', '<w:root %s><w:r><w:t>a<y</w:t></w:r><w:r><w:t>NEXT</w:t></w:r></w:root>' % W),
    ('tag_then_close_parent', '<w:root %s><w:r><w:t>a<x>b</w:t></w:r></w:root>' % W),
    ('real_value_amp', '<w:root %s><w:r><w:t xml:space="preserve">Company: AT&amp;T Inc</w:t></w:r></w:root>' % W),
    ('double_lt_text', '<w:root %s><w:r><w:t>a &lt;&lt; b</w:t></w:r></w:root>' % W),
    ('tag_with_attrs', '<w:root %s><w:r><w:t>a<b attr="v&amp;x">c</b>d</w:t></w:r></w:root>' % W),
    ('mismatch_deep', '<w:root %s><w:r><w:t>a<b>c</w:t>d</w:r>e</w:root>' % W),
]
for name, src in cases:
    p = etree.XMLParser(recover=True)
    t = etree.fromstring(src.encode(), parser=p)
    out = etree.tostring(t, encoding='unicode') if t is not None else 'NONE'
    sys.stdout.write('== %s => %s\n' % (name, out))
