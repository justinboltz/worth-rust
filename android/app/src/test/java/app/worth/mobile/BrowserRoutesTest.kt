package app.worth.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BrowserRoutesTest {
    private val origin = "http://localhost:3000"

    @Test fun browserOnlyFlowsUseTheConfiguredServer() {
        assertTrue(BrowserRoutes.handles("$origin/bank_connections/new", origin))
        assertTrue(BrowserRoutes.handles("$origin/transactions.csv?q=groceries", origin))
        assertTrue(BrowserRoutes.handles("$origin/transactions?format=csv&q=groceries", origin))
        assertFalse(BrowserRoutes.handles("$origin/transactions?q=groceries", origin))
        assertFalse(BrowserRoutes.handles("$origin/imports/new", origin))
    }

    @Test fun otherOriginsAndMalformedLinksDoNotMatch() {
        assertFalse(BrowserRoutes.handles("https://localhost:3000/bank_connections/new", origin))
        assertFalse(BrowserRoutes.handles("http://localhost:4000/bank_connections/new", origin))
        assertFalse(BrowserRoutes.handles("http://localhost.example:3000/bank_connections/new", origin))
        assertFalse(BrowserRoutes.handles("http://user@localhost:3000/bank_connections/new", origin))
        assertFalse(BrowserRoutes.handles("not a URL", origin))
    }

    @Test fun equivalentHttpsOriginsMatch() {
        assertTrue(BrowserRoutes.handles("https://WORTH.EXAMPLE:443/transactions.csv", "https://worth.example"))
        assertFalse(BrowserRoutes.handles("https://worth.example:8443/transactions.csv", "https://worth.example"))
    }
}
