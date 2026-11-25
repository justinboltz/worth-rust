package app.worth.mobile

import java.net.URI

/** Flows that currently require a full browser, on Worth's own server only. */
object BrowserRoutes {
    fun handles(location: String, startLocation: String): Boolean = runCatching {
        val target = URI(location)
        val origin = URI(startLocation)
        val sameOrigin = target.scheme.equals(origin.scheme, ignoreCase = true) &&
            target.host?.equals(origin.host, ignoreCase = true) == true &&
            port(target) == port(origin) && target.rawUserInfo == null
        val csvExport = target.path == "/transactions.csv" ||
            (target.path == "/transactions" && target.rawQuery?.split('&')?.contains("format=csv") == true)
        sameOrigin && (target.path == "/bank_connections/new" || csvExport)
    }.getOrDefault(false)

    private fun port(uri: URI): Int = if (uri.port != -1) uri.port else if (uri.scheme.equals("https", true)) 443 else 80
}
