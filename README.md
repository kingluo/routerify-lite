# Routerify lite

Routerify-lite is a simplified but faster version of [Routerify](https://github.com/routerify/routerify).

It only provides below functions:
* path matching
* error handling

## Why not Routerify?

Routerify is an excellent lib! But sometimes it performs not so well.
So I rewrite some code paths to improve performance.
If you need the full functions, you must use Routerify.

## Why not warp?

Warp is fast. But due to its linear scan path matching,
it would be as slow as the number of API combination increase.
However, Routerify uses RegexSet, which could match the URI path towards the pattern list in one scan.

For example, I add 50 random URIs to match, then routerify-lite is better than warp.

Check the code here:

https://gist.github.com/kingluo/8ccd88b53e9d2878391dbb91ad1f4751

`wrk -t2 -c100 -d60s http://test1:8080/hello/world`

![Performance2](performance2.png)

## Performance

`wrk -t2 -c100 -d60s http://test1:8080`

![Performance](performance.png)

## Difference between Routerify and Routerify-lite

* Routerify uses `*` to matching anything, but Routerify-lite uses `.*`
* Routerify adds default 404 error response, while Routerify-lite needs the user to add manually

