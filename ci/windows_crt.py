"""Reject VC/UCRT runtime and third-party DLL imports from Windows release binaries."""
import re
import struct

SYSTEM_DLLS=set('''advapi32.dll authz.dll avrt.dll bcrypt.dll bcryptprimitives.dll
cabinet.dll cfgmgr32.dll combase.dll comctl32.dll comdlg32.dll crypt32.dll cryptbase.dll
cryptnet.dll cryptsp.dll d2d1.dll d3d9.dll d3d11.dll d3d12.dll d3dcompiler_47.dll
dbghelp.dll dhcpcsvc.dll dhcpcsvc6.dll dnsapi.dll dpapi.dll dwrite.dll dwmapi.dll
dxcore.dll dxgi.dll gdi32.dll gdiplus.dll imm32.dll iphlpapi.dll kernel32.dll kernelbase.dll
mf.dll mfplat.dll mfreadwrite.dll mfsrcsnk.dll msacm32.dll msasn1.dll msimg32.dll
netapi32.dll normaliz.dll ntdll.dll ole32.dll oleacc.dll oleaut32.dll opengl32.dll
pdh.dll powrprof.dll propsys.dll psapi.dll rpcrt4.dll secur32.dll setupapi.dll
shell32.dll shlwapi.dll user32.dll userenv.dll uxtheme.dll version.dll wevtapi.dll
windowscodecs.dll winhttp.dll wininet.dll winmm.dll winspool.drv wintrust.dll
ws2_32.dll wtsapi32.dll'''.split())


def imports(blob):
    if blob[:2]!=b'MZ':raise ValueError('Expected a PE image')
    pe=struct.unpack_from('<I',blob,0x3c)[0]
    if blob[pe:pe+4]!=b'PE\0\0':raise ValueError('Invalid PE signature')
    count=struct.unpack_from('<H',blob,pe+6)[0]
    size=struct.unpack_from('<H',blob,pe+20)[0]
    optional=pe+24
    if struct.unpack_from('<H',blob,optional)[0]!=0x20b:raise ValueError('Expected PE32+')
    image_base=struct.unpack_from('<Q',blob,optional+24)[0]
    sections=[]
    for index in range(count):
        offset=optional+size+40*index
        virtual_size,address,raw_size,raw=struct.unpack_from('<IIII',blob,offset+8)
        sections.append((address,max(virtual_size,raw_size),raw))
    def position(rva):
        for address,length,raw in sections:
            if address<=rva<address+length:return raw+rva-address
        raise ValueError('PE import RVA is outside sections')
    def name(rva):
        start=position(rva)
        return blob[start:blob.index(b'\0',start)].decode('ascii').lower()
    result=set()
    rva=struct.unpack_from('<I',blob,optional+112+8)[0]
    if rva:
        offset=position(rva)
        while True:
            entry=struct.unpack_from('<IIIII',blob,offset)
            if not any(entry):break
            result.add(name(entry[3]));offset+=20
    rva=struct.unpack_from('<I',blob,optional+112+13*8)[0]
    if rva:
        offset=position(rva)
        while True:
            entry=struct.unpack_from('<IIIIIIII',blob,offset)
            if not any(entry):break
            result.add(name(entry[1] if entry[0]&1 else entry[1]-image_base));offset+=32
    return sorted(result)


def verify(path):
    dependencies=imports(path.read_bytes())
    crt=[name for name in dependencies if re.match(r'(vcruntime|msvcp|msvcr\d|mfc\d|concrt|vcomp|api-ms-win-crt-)',name) or name in ['ucrtbase.dll','ucrtbased.dll','msvcrt.dll']]
    if crt:raise ValueError(f'{path}: dynamically linked CRT imports: {crt}; compile all objects with /MT')
    external=[name for name in dependencies if name not in SYSTEM_DLLS and not name.startswith(('api-ms-win-','ext-ms-win-'))]
    if external:raise ValueError(f'{path}: non-system DLL imports: {external}')
    return dependencies
