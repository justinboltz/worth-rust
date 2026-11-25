package app.worth.mobile

import android.app.Application
import dev.hotwire.core.config.Hotwire
import dev.hotwire.core.turbo.config.PathConfiguration
import dev.hotwire.core.turbo.webview.HotwireWebView
import dev.hotwire.navigation.config.defaultFragmentDestination
import dev.hotwire.navigation.config.registerFragmentDestinations
import dev.hotwire.navigation.config.registerRouteDecisionHandlers
import dev.hotwire.navigation.routing.AppNavigationRouteDecisionHandler
import dev.hotwire.navigation.routing.BrowserTabRouteDecisionHandler
import dev.hotwire.navigation.routing.SystemNavigationRouteDecisionHandler

class WorthApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Hotwire.config.applicationUserAgentPrefix = "Worth Android;"
        Hotwire.config.webViewDebuggingEnabled = BuildConfig.DEBUG
        Hotwire.config.makeCustomWebView = { context ->
            HotwireWebView(context).apply { settings.allowFileAccess = false }
        }
        Hotwire.defaultFragmentDestination = WorthWebFragment::class
        Hotwire.registerFragmentDestinations(WorthWebFragment::class)
        Hotwire.registerRouteDecisionHandlers(
            WorthBrowserRouteHandler(),
            AppNavigationRouteDecisionHandler(),
            BrowserTabRouteDecisionHandler(),
            SystemNavigationRouteDecisionHandler()
        )
        Hotwire.loadPathConfiguration(this, PathConfiguration.Location(assetFilePath = "path-configuration.json"))
    }
}
