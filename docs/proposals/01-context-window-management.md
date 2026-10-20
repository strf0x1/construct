Proposal 1

I've taken some ideas from all over the place in https://github.com/strf0x1/substrate_v3, and it's generally worked out pretty well. For construct to work well, it needs a more sophisticated context window manager. Here are some of the resources I've found along the way that might help:

- this is newly released: https://github.com/VictorTaelin/uniichat . Victor plays with the idea of no sessions at all, just one big chat history and moderately aggressive pruning. I havent investigated thoroughly yet, but i took some of his initial recipe, weighed each of the ideas, and took what made sense for substrate_v3. recipe: https://github.com/VictorTaelin/OptMem

- opencode released the durable project. this is similar to how the journal works in substrate_v3. the one thing im not convinced of is, if you use this as part of your compaction strategy, will agents actually recall text from the journal or not. i havent seen a lot of evidence yet that it is used in practice. regardless, there are a bunch of other advantages to having a durable journal, compaction is just one thing i considered.

- i had a paper i found with really good research on compaction windows and strategies. need to dig it up.

- a lot of really good reviews of deepseek harness. should dig up how their compaction strategy works. 
