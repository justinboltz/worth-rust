package app.worth.mobile

import dev.hotwire.core.turbo.visit.VisitProposal
import dev.hotwire.navigation.activities.HotwireActivity
import dev.hotwire.navigation.navigator.NavigatorConfiguration
import dev.hotwire.navigation.routing.Router

class WorthBrowserRouteHandler : Router.RouteDecisionHandler {
    override val name = "worth-browser-flows"

    override fun matches(proposal: VisitProposal, configuration: NavigatorConfiguration) =
        BrowserRoutes.handles(proposal.location, configuration.startLocation)

    override fun handle(proposal: VisitProposal, configuration: NavigatorConfiguration, activity: HotwireActivity): Router.Decision {
        BrowserLauncher.open(activity, proposal.location)
        return Router.Decision.CANCEL
    }
}
