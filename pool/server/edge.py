"""TLS transport for SMW/1. Each client gets an isolated, local verifier session."""
import asyncio
import ssl
import sys

async def copy(src, dst):
    while True:
        data=await asyncio.wait_for(src.read(8192),20)
        if not data:return
        dst.write(data)
        await asyncio.wait_for(dst.drain(),5)

async def main():
    ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version=ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(sys.argv[1],sys.argv[2])
    slots=asyncio.Semaphore(64)
    async def accept(rd,wr):
        backend=None
        tasks=[]
        try:
            if slots.locked():return
            async with slots:
                br,backend=await asyncio.open_unix_connection("/run/svy-pool-session.sock")
                tasks=[asyncio.create_task(copy(rd,backend)),asyncio.create_task(copy(br,wr))]
                await asyncio.wait(tasks,return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in tasks:task.cancel()
            if tasks:await asyncio.gather(*tasks,return_exceptions=True)
            if backend:backend.close()
            wr.close()
    server=await asyncio.start_server(accept,"0.0.0.0",18444,ssl=ctx,ssl_handshake_timeout=4,limit=16384)
    async with server:await server.serve_forever()
if __name__=="__main__":asyncio.run(main())
