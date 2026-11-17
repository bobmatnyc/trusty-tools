"""Code mode's page tools, registered on a FastMCP server; contract docs/activation-codes.md, "MCP tools".

Split from agent_mcp for the SLOC cap (#11286); agent_mcp re-exports ``add_page_tools``.
"""

from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP


def add_page_tools(
    server: FastMCP, call: Callable[..., dict[str, Any]], root: Path | None = None
) -> None:
    """Register code mode's page tools; contract "MCP tools", "Code mode".

    Test: `test_end_to_end_page_session_through_the_mcp_server`.
    """

    @server.tool()
    def browser_tabs() -> dict[str, Any]:
        """List the tabs this grant may address, with page access, origin and title."""
        return call("tabs")

    @server.tool()
    def browser_select(tab: int) -> dict[str, Any]:
        """Lease a tab by id for this session (does not focus it); returns the lease."""
        return call("select", tab=tab)

    @server.tool()
    def browser_switch(lease: str) -> dict[str, Any]:
        """Bring the leased tab to the front of its window and focus that window."""
        return call("switch", lease=lease)

    @server.tool()
    def page_read(
        lease: str,
        scope_ref: str | None = None,
        max_chars: int = 50000,
        viewport_only: bool = False,
        include_text: bool = False,
    ) -> dict[str, Any]:
        """Read the leased tab's page: elements with refs for click and type.

        ``scope_ref`` reads only that element's subtree; ``viewport_only`` keeps
        elements on screen; ``include_text`` adds the page (or scope) text;
        ``max_chars`` (1000-250000) bounds the reply. Elements nearest the
        viewport are kept first and ``truncated`` reports any cut (#11356).
        """
        return call(
            "page_read",
            lease=lease,
            scope_ref=scope_ref,
            max_chars=max_chars,
            viewport_only=viewport_only,
            include_text=include_text,
        )

    @server.tool()
    def page_find(
        lease: str,
        role: str | None = None,
        name: str | None = None,
        text: str | None = None,
        within_ref: str | None = None,
        limit: int = 10,
    ) -> dict[str, Any]:
        """Find elements by ``role`` (exact), ``name`` and ``text`` (substrings, any case).

        Give at least one of role, name and text. ``text`` matches the
        element's own text or its container's; ``within_ref`` searches only
        inside that element. Each match carries ``in``: its nearest list, row,
        dialog, form, navigation or main, with name and ref (#11357).
        """
        return call(
            "page_find",
            lease=lease,
            role=role,
            name=name,
            text=text,
            within_ref=within_ref,
            limit=limit,
        )

    @server.tool()
    def page_click(
        lease: str, ref: str, request_id: str, dialog: str = "dismiss"
    ) -> dict[str, Any]:
        """Click the element a page_read returned as ``ref`` with a trusted mouse click.

        ``dialog`` ("accept" or "dismiss") answers a confirm, prompt or
        beforeunload dialog the click opens; an alert is always acknowledged and
        the reply names the dialog (#11294). Reuse the request ID to retry.
        """
        return call(
            "page_click", lease=lease, ref=ref, request_id=request_id, dialog=dialog
        )

    @server.tool()
    def page_hover(lease: str, ref: str, request_id: str) -> dict[str, Any]:
        """Move the pointer onto the element a page_read returned as ``ref``.

        Why: menus and tooltips open on hover (#11287). What: fires pointer
        and mouse over/enter/move on the element, and out/leave on the one
        hovered before. The events are synthetic, so CSS ``:hover`` does not
        match. Reuse the request ID to retry.
        Test: `test_end_to_end_page_session_through_the_mcp_server`.
        """
        return call("page_hover", lease=lease, ref=ref, request_id=request_id)

    @server.tool()
    def page_drag(lease: str, ref: str, target: str, request_id: str) -> dict[str, Any]:
        """Drag the element ``ref`` onto ``target`` (both refs from one page_read).

        ``dropped`` is false when the target did not accept the drop. Reuse the
        request ID to retry. See #11288.
        """
        return call(
            "page_drag", lease=lease, ref=ref, target=target, request_id=request_id
        )

    @server.tool()
    def page_type(lease: str, ref: str, text: str, request_id: str) -> dict[str, Any]:
        """Replace the text of an editable element named by ``ref``, one keystroke per character.

        A newline presses Enter (a single-line field submits its form) and a tab presses Tab.
        For a dropdown use page_select.
        """
        return call("page_type", lease=lease, ref=ref, text=text, request_id=request_id)

    @server.tool()
    def page_scroll(
        lease: str,
        request_id: str,
        to: str = "down",
        ref: str | None = None,
        wait_ms: int = 1000,
    ) -> dict[str, Any]:
        """Scroll the leased tab, then wait up to ``wait_ms`` for new content.

        Why: offscreen and infinite-scroll content loads only on scroll (#11291).
        What: ``to`` is down or up (90% of the viewport), top, bottom, or
        element with ``ref`` from the latest page_read. The result carries
        scroll_y, scroll_height, viewport_height, grew (content was added) and
        at_end. For a feed, repeat ``to="bottom"`` until grew is false, then
        page_read again. A ref survives reads while its element keeps its role
        and name; a removed or changed element answers StaleElement (#11355).
        Reuse the request ID to retry.
        Test: `test_end_to_end_page_session_through_the_mcp_server`.
        """
        return call(
            "page_scroll",
            lease=lease,
            to=to,
            ref=ref,
            wait_ms=wait_ms,
            request_id=request_id,
        )

    @server.tool()
    def page_press(
        lease: str,
        key: str,
        request_id: str,
        ref: str | None = None,
        modifiers: list[str] | None = None,
    ) -> dict[str, Any]:
        """Press one key, with optional modifiers, on ``ref`` or the focused element.

        Why: pages submit on Enter and move focus on Tab, and page handlers
        listen for key events (#11286). What: ``key`` is Enter, Tab, Escape,
        Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home,
        End, PageUp, PageDown, Space or one printable character; ``modifiers``
        is any of Shift, Control, Alt, Meta. Omit ``ref`` to press on the
        focused element. Reuse the request ID to retry.
        Test: `test_end_to_end_page_session_through_the_mcp_server`.
        """
        return call(
            "page_press",
            lease=lease,
            ref=ref,
            key=key,
            modifiers=modifiers or [],
            request_id=request_id,
        )

    @server.tool()
    def page_select(
        lease: str, ref: str, value: str, request_id: str
    ) -> dict[str, Any]:
        """Choose an option of the native ``<select>`` named by ``ref``.

        Why: a native dropdown ignores clicks on its options and is not a
        typing target (#11285). What: ``value`` matches an option's value,
        else its label, from the ``options`` page_read lists. The reply's
        ``changed`` is false when the option was already selected or a page
        handler reverted it, and ``value`` is the select's value afterwards.
        ``NotSelectable`` and ``NoSuchOption`` refuse without changing the
        page. Reuse the request ID to retry.
        Test: `test_end_to_end_page_session_through_the_mcp_server`.
        """
        return call(
            "page_select", lease=lease, ref=ref, value=value, request_id=request_id
        )

    @server.tool()
    def page_navigate(lease: str, url: str, request_id: str) -> dict[str, Any]:
        """Navigate the leased tab to an https URL (or http://127.0.0.1)."""
        return call("page_navigate", lease=lease, url=url, request_id=request_id)

    @server.tool()
    def page_back(lease: str, request_id: str) -> dict[str, Any]:
        """Go back one entry in the leased tab's history; NoHistory when there is none."""
        return call("page_back", lease=lease, request_id=request_id)

    @server.tool()
    def page_forward(lease: str, request_id: str) -> dict[str, Any]:
        """Go forward one entry in the leased tab's history; NoHistory when there is none."""
        return call("page_forward", lease=lease, request_id=request_id)

    @server.tool()
    def page_reload(lease: str, request_id: str) -> dict[str, Any]:
        """Reload the leased tab's page."""
        return call("page_reload", lease=lease, request_id=request_id)

    @server.tool()
    def page_upload(
        lease: str, ref: str, paths: list[str], request_id: str
    ) -> dict[str, Any]:
        """Set local files on the <input type=file> named by ``ref``; the page sees a user's pick.

        ``paths`` name files under the server's --upload-root (relative to it, or
        absolute inside it), at most 10 and 512 KiB in total. Without
        --upload-root, or for a symlink or a path outside the root, it refuses.
        """
        from trusty_browser.agent_mcp import upload_files  # deferred: agent_mcp imports this module

        return call(
            "page_upload",
            lease=lease,
            ref=ref,
            files=upload_files(paths, root),
            request_id=request_id,
        )

    @server.tool()
    def page_downloads(lease: str) -> dict[str, Any]:
        """List the newest downloads started from the leased tab's origin, with path and state."""
        return call("page_downloads", lease=lease)

    @server.tool()
    def browser_release(lease: str) -> dict[str, Any]:
        """Release a lease taken with browser_select."""
        return call("release", lease=lease)
