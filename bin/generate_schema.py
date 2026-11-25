#!/usr/bin/env python3
"""One-time schema translation. Usage: generate_schema.py /path/to/Budget/db/schema.rb"""
import re, json, sys
from pathlib import Path
source=Path(sys.argv[1]).read_text(); out=[]; tables={}; indices=[]; constraints=[]
for table,body in re.findall(r'create_table "(\w+)".*?do \|t\|\n(.*?)\n  end',source,re.S):
 columns=['id BIGSERIAL PRIMARY KEY']; tables[table]=['id']
 for line in body.splitlines():
  m=re.match(r'\s+t\.(string|text|bigint|integer|boolean|decimal|date|datetime|jsonb) "(\w+)"(.*)',line)
  if m:
   typ,name,opts=m.groups(); tables[table].append(name)
   sqltype={'string':'TEXT','text':'TEXT','bigint':'BIGINT','integer':'INTEGER','boolean':'BOOLEAN','date':'DATE','datetime':'TIMESTAMP(6)','jsonb':'JSONB','decimal':'NUMERIC'}[typ]
   if typ=='decimal': sqltype+='('+','.join(re.findall(r'(?:precision|scale): (\d+)',opts))+')'
   col=f'"{name}" {sqltype}'
   default=re.search(r'default: ("[^"]*"|true|false|\{\}|-?\d+)',opts)
   if default:
    val=default[1]; val="'"+val[1:-1].replace("'","''")+"'" if val.startswith('"') else "'{}'::jsonb" if val=='{}' else val
    col+=' DEFAULT '+val
   if name in ('created_at','updated_at'): col+=' DEFAULT CURRENT_TIMESTAMP'
   if 'null: false' in opts: col+=' NOT NULL'
   columns.append(col)
  m=re.match(r'\s+t.index (\[.*?\]), name: "([^"]+)"(.*)',line)
  if m:
   cols,name,opts=m.groups(); cols=', '.join('"'+v+'"' for v in json.loads(cols)); where=re.search('where: "(.*)"',opts)
   indices.append(f'CREATE {"UNIQUE " if "unique: true" in opts else ""}INDEX "{name}" ON "{table}" ({cols})'+(' WHERE '+where[1] if where else '')+';')
  m=re.match(r'\s+t.check_constraint "(.*)", name: "(.*)"',line)
  if m: constraints.append(f'ALTER TABLE "{table}" ADD CONSTRAINT "{m[2]}" CHECK ({m[1]});')
 out.append(f'CREATE TABLE "{table}" (\n  '+',\n  '.join(columns)+'\n);')
for table,target,opts in re.findall(r'add_foreign_key "(\w+)", "(\w+)"([^\n]*)',source):
 col=re.search('column: "(.*?)"',opts); field=col[1] if col else target[:-3]+'y_id' if target.endswith('ies') else target[:-1]+'_id'
 delete=' ON DELETE CASCADE' if ':cascade' in opts else ' ON DELETE SET NULL' if ':nullify' in opts else ''
 constraints.append(f'ALTER TABLE "{table}" ADD FOREIGN KEY ("{field}") REFERENCES "{target}"(id){delete};')
root=Path(__file__).resolve().parent.parent
(root/'migrations/0001_worth.sql').write_text('-- Translated from the Worth Rails working schema, preserving IDs and precision.\n\n'+'\n\n'.join(out+indices+constraints)+'\n')
(root/'src/schema.json').write_text(json.dumps(tables,indent=2)+'\n')
