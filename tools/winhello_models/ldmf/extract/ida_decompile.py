
import sys, os
import idapro
db=sys.argv[1]; out=sys.argv[2]; targets=sys.argv[3:]
idapro.open_database(db, False)
import ida_hexrays, ida_funcs, idautils, ida_name, idc, ida_bytes
ida_hexrays.init_hexrays_plugin()
n=0
for ea,name in list(idautils.Names()):
    if name.startswith('__xmm@') or name.startswith('__ymm@'):
        ida_name.set_name(ea, 'XMMC_%x'%ea, ida_name.SN_NOWARN|ida_name.SN_FORCE); n+=1
print('renamed',n)
os.makedirs(out, exist_ok=True)
names={ea:ida_funcs.get_func_name(ea) for ea in idautils.Functions()}
for t in targets:
    for ea,nm in names.items():
        dn=idc.demangle_name(nm, idc.get_inf_attr(idc.INF_SHORT_DN)) or nm
        if t in dn:
            cf=ida_hexrays.decompile(ea)
            open(os.path.join(out,'%x.c'%ea),'w').write('// %x %s\n'%(ea,dn)+str(cf))
            print('%x'%ea, dn[:100])
idapro.close_database(False)
