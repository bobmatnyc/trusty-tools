Fixed
- trusty-analyze now honours `TRUSTY_SEARCH_SOCKET`: every trusty-search call goes to that socket, or the standard path when it is unset. A missing socket is an error naming the path (#9214).
