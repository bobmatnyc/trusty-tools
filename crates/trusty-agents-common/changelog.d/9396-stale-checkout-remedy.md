Fixed
- A missing or unusable file in content served by a trusty-tools checkout now names that checkout and says to run `git pull` there, or to run tm from outside it. These errors no longer claim that `tm content update` fixes them, because a checkout serves its own working tree (#9396).
- Every "no instructional content is installed" error names a manual install that needs no GitHub API call: `gh release download <tag> --repo bobmatnyc/trusty-tools`, then `tm content install --from <dir>/<tag>.tar.gz` (#9396).
