import json
from http.cookiejar import CookieJar
from urllib.error import HTTPError
from urllib.request import HTTPCookieProcessor, Request, build_opener

import pytest

from live_test import qa_site


def test_cookie_subject_is_checked_when_reading_and_committing():
    with qa_site() as site:
        for account, url in site.login.items():
            client = build_opener(HTTPCookieProcessor(CookieJar()))
            assert account in client.open(url).read().decode()
            assert json.load(client.open(site.base + "/identity")) == {
                "account": account
            }
            for expected in ("different", None, account):
                headers = {"Origin": site.origin}
                if expected:
                    headers["X-Expected-Account"] = expected
                req = Request(site.base + "/apply", b"marker", headers)
                if expected == account:
                    assert client.open(req).status == 200
                else:
                    with pytest.raises(HTTPError) as err:
                        client.open(req)
                    assert err.value.code == 403
        assert site.journal == {account: ["marker"] for account in site.login}
        with pytest.raises(HTTPError) as err:
            build_opener().open(site.base + "/identity")
        assert err.value.code == 403
