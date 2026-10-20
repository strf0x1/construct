Proposal 2 - Durable Journal

substrate_v3 has a durable journal, something i got inspired by reading one of the OMP author's blog posts. then i saw opencode 2.0 was doing the same thing. there is more than one advantage to having a transaction of every event and result that occurred in the harness, including rewinds, fault tolerance through crashes, auditing, and agent history retrieval. the history thing is why i originally built chatstrata, but chatstrata has SQL for an interface and substrate_v3 journal does not. so i added the MCP of chatastrata to construct as an easy win early on in the project. it can be removed, and a dedicated journal could be used, and if we'd like a SQL interface ontop to make it easy for agents crawl through. 

substrate_v3 uses sqlite WAL for the journal. this seems fine. but could probably do a better job of managing multiple sessions right out the gate with something like that for construct. 
